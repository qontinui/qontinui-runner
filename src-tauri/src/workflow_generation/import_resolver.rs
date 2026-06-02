//! Deterministic, language-aware import resolution for the `Ξ_AST` code graph.
//!
//! This is the **cheap middle tier** between syntactic `Ξ_AST` (raw module strings)
//! and the LSP `Ξ_Type` layer — no language server, no model. It binds each
//! [`ImportEdge::to_module`] specifier to an actual repo-relative file path
//! (`resolved_target`) and records *how* it was bound ([`ResolutionKind`]).
//!
//! Pragmatic v1 (Q1 resolved = pragmatic; mark `Unresolved` honestly):
//! - **TS/JS:** relative (`./`, `../`) with extension inference + `index.*`;
//!   tsconfig `paths`/`baseUrl` aliases (the pre-resolved import map); bare → `External`.
//! - **Python:** dotted module → `pkg/mod.py` / `pkg/__init__.py`; relative (`.`, `..`)
//!   against the file's package; stdlib/site-packages → `External`.
//! - **Rust:** `use`/`mod` within the crate (`mod foo;` → `foo.rs`/`foo/mod.rs`);
//!   `crate::`/`super::`/`self::` prefixes; extern crates → `External`.
//!
//! All resolved targets use the SAME normalization as `FileNode.path`
//! (forward slashes, project-prefix-stripped). The resolver runs **once during
//! `CodeGraph::build`**, producing the import map threaded into blast-radius.

use super::code_graph::{CodeGraph, ResolutionKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// The deterministic resolver. Holds the set of known repo-relative file paths
/// (the only files the graph saw) plus the parsed tsconfig alias maps.
pub struct ImportResolver {
    /// Set of repo-relative file paths present in the graph (forward-slash normalized).
    known_files: HashSet<String>,
    /// Lowercased extension-stripped lookup: stem-with-dirs -> full path, for fast
    /// "does `a/b/c` (without extension) name a real file" checks.
    /// e.g. `src/auth` -> `src/auth.ts`.
    by_stem: HashMap<String, String>,
    /// Directory paths that contain an `index.*` (or `__init__.py`) entry file ->
    /// the entry file path. e.g. `src/components` -> `src/components/index.ts`.
    dir_index: HashMap<String, String>,
    /// tsconfig alias maps, ordered most-specific (deepest dir) first.
    /// Each entry: (config_dir, base_url_dir, [(alias_prefix, [target_prefixes])]).
    tsconfigs: Vec<TsconfigScope>,
}

struct TsconfigScope {
    /// Repo-relative directory containing the tsconfig.json (forward-slash, no trailing slash).
    dir: String,
    /// Repo-relative baseUrl directory (resolved against `dir`), if present.
    base_url: Option<String>,
    /// Alias entries: (pattern, [substitutions]) from compilerOptions.paths.
    paths: Vec<(String, Vec<String>)>,
}

/// Output of a single resolution: the bound file (if any) and how it was bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub resolved_target: Option<String>,
    pub resolution: ResolutionKind,
}

const TS_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs"];

impl ImportResolver {
    /// Build a resolver from the graph's file list and the project root (to read tsconfigs).
    pub fn new(graph: &CodeGraph, project_path: &Path) -> Self {
        let mut known_files = HashSet::new();
        let mut by_stem: HashMap<String, String> = HashMap::new();
        let mut dir_index: HashMap<String, String> = HashMap::new();

        for f in &graph.files {
            let path = f.path.clone();
            known_files.insert(path.clone());

            // Stem (path without the final extension), for extension-inference lookups.
            if let Some(stem) = strip_known_ext(&path) {
                by_stem.entry(stem).or_insert_with(|| path.clone());
            }

            // Directory index entries: index.ts/tsx/js/jsx/__init__.py.
            let (dir, file_name) = split_dir_file(&path);
            let lower = file_name.to_ascii_lowercase();
            let is_ts_index = TS_EXTS.iter().any(|e| lower == format!("index.{}", e));
            if is_ts_index {
                dir_index.entry(dir).or_insert_with(|| path.clone());
            }
        }

        let tsconfigs = discover_tsconfigs(graph, project_path);

        ImportResolver {
            known_files,
            by_stem,
            dir_index,
            tsconfigs,
        }
    }

    /// Resolve a single (`from_file`, `specifier`, `language`) triple.
    pub fn resolve(&self, from_file: &str, specifier: &str, language: &str) -> Resolution {
        match language {
            "typescript" | "javascript" => self.resolve_ts(from_file, specifier),
            "python" => self.resolve_python(from_file, specifier),
            "rust" => self.resolve_rust(from_file, specifier),
            _ => Resolution {
                resolved_target: None,
                resolution: ResolutionKind::Unresolved,
            },
        }
    }

    /// Resolve every import edge in the graph in place. Runs once during build.
    pub fn resolve_graph(&self, graph: &mut CodeGraph) {
        // Map file_path -> language for quick lookup of the importing file's language.
        let lang_of: HashMap<&str, &str> = graph
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_str()))
            .collect();

        // Clone the per-file languages we need first to avoid borrow conflicts.
        let langs: Vec<String> = graph
            .imports
            .iter()
            .map(|imp| {
                lang_of
                    .get(imp.from_file.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| language_from_path(&imp.from_file))
            })
            .collect();

        for (imp, lang) in graph.imports.iter_mut().zip(langs) {
            let r = self.resolve(&imp.from_file, &imp.to_module, &lang);
            imp.resolved_target = r.resolved_target;
            imp.resolution = r.resolution;
        }
    }

    // ---- TS / JS -----------------------------------------------------------

    fn resolve_ts(&self, from_file: &str, specifier: &str) -> Resolution {
        // Relative specifier.
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier == "." {
            let base_dir = parent_dir(from_file);
            let joined = normalize_join(&base_dir, specifier);
            if let Some(target) = self.resolve_ts_path(&joined) {
                let kind = if self.known_files.contains(&target) || self.by_stem_hit(&joined) {
                    ResolutionKind::Relative
                } else {
                    ResolutionKind::PackageIndex
                };
                // If we landed via a directory index, label PackageIndex; else Relative.
                let kind = if self
                    .dir_index
                    .get(&joined)
                    .map(|t| t == &target)
                    .unwrap_or(false)
                {
                    ResolutionKind::PackageIndex
                } else {
                    kind
                };
                return Resolution {
                    resolved_target: Some(target),
                    resolution: kind,
                };
            }
            // A relative path that doesn't resolve is an internal coverage hole.
            return Resolution {
                resolved_target: None,
                resolution: ResolutionKind::Unresolved,
            };
        }

        // Bare specifier — try tsconfig path aliases first.
        if let Some(target) = self.resolve_tsconfig_alias(from_file, specifier) {
            return Resolution {
                resolved_target: Some(target),
                resolution: ResolutionKind::TsconfigPath,
            };
        }

        // Otherwise a third-party / stdlib package — honestly external.
        Resolution {
            resolved_target: None,
            resolution: ResolutionKind::External,
        }
    }

    fn by_stem_hit(&self, path_no_ext: &str) -> bool {
        self.by_stem.contains_key(path_no_ext)
    }

    /// Resolve a TS module path (no extension yet) to a concrete file via extension
    /// inference then directory-index fallback. `path` is repo-relative, normalized.
    fn resolve_ts_path(&self, path: &str) -> Option<String> {
        // Exact file (specifier already had an extension).
        if self.known_files.contains(path) {
            return Some(path.to_string());
        }
        // Extension inference: path + .ts/.tsx/.js/.jsx/...
        for ext in TS_EXTS {
            let candidate = format!("{}.{}", path, ext);
            if self.known_files.contains(&candidate) {
                return Some(candidate);
            }
        }
        // Directory index: path/index.*
        if let Some(idx) = self.dir_index.get(path) {
            return Some(idx.clone());
        }
        None
    }

    /// Try each tsconfig scope covering `from_file`, most-specific first.
    fn resolve_tsconfig_alias(&self, from_file: &str, specifier: &str) -> Option<String> {
        for scope in &self.tsconfigs {
            if !path_in_dir(from_file, &scope.dir) {
                continue;
            }
            // 1) Explicit `paths` aliases.
            for (pattern, subs) in &scope.paths {
                if let Some(captured) = match_alias(pattern, specifier) {
                    for sub in subs {
                        let replaced = apply_alias(sub, &captured);
                        // Targets in `paths` are relative to baseUrl (or config dir).
                        let base = scope.base_url.clone().unwrap_or_else(|| scope.dir.clone());
                        let joined = normalize_join(&base, &replaced);
                        if let Some(target) = self.resolve_ts_path(&joined) {
                            return Some(target);
                        }
                    }
                }
            }
            // 2) Bare baseUrl resolution (non-relative import resolved against baseUrl).
            if let Some(base) = &scope.base_url {
                let joined = normalize_join(base, specifier);
                if let Some(target) = self.resolve_ts_path(&joined) {
                    return Some(target);
                }
            }
        }
        None
    }

    // ---- Python ------------------------------------------------------------

    fn resolve_python(&self, from_file: &str, specifier: &str) -> Resolution {
        // Relative import: leading dots (`.mod`, `..pkg.mod`, `.`).
        if specifier.starts_with('.') {
            let dots = specifier.chars().take_while(|c| *c == '.').count();
            let rest = &specifier[dots..];
            // Start from the importing file's directory, then go up (dots-1) levels.
            let mut base = parent_dir(from_file);
            for _ in 1..dots {
                base = parent_dir(&base);
            }
            let sub = rest.replace('.', "/");
            let target_dir = if sub.is_empty() {
                base.clone()
            } else {
                normalize_join(&base, &sub)
            };
            if let Some(t) = self.resolve_python_path(&target_dir) {
                return Resolution {
                    resolved_target: Some(t),
                    resolution: ResolutionKind::PythonModule,
                };
            }
            // A relative python import that doesn't bind is an internal hole.
            return Resolution {
                resolved_target: None,
                resolution: ResolutionKind::Unresolved,
            };
        }

        // Absolute dotted module: try each package root by progressively longer prefixes.
        let as_path = specifier.replace('.', "/");
        if let Some(t) = self.resolve_python_path(&as_path) {
            return Resolution {
                resolved_target: Some(t),
                resolution: ResolutionKind::PythonModule,
            };
        }
        // Try treating the top-level package as a package root anchored under known dirs.
        if let Some(t) = self.resolve_python_under_roots(&as_path) {
            return Resolution {
                resolved_target: Some(t),
                resolution: ResolutionKind::PythonModule,
            };
        }

        // Not found anywhere in-repo → stdlib / site-packages → External.
        Resolution {
            resolved_target: None,
            resolution: ResolutionKind::External,
        }
    }

    /// Resolve a slash-joined python module path: `pkg/mod.py` or `pkg/mod/__init__.py`.
    fn resolve_python_path(&self, path: &str) -> Option<String> {
        let as_file = format!("{}.py", path);
        if self.known_files.contains(&as_file) {
            return Some(as_file);
        }
        let as_init = format!("{}/__init__.py", path);
        if self.known_files.contains(&as_init) {
            return Some(as_init);
        }
        None
    }

    /// Try resolving a dotted module under any known top-level directory root, so
    /// `app.models` binds to `backend/app/models.py` when the package root is nested.
    fn resolve_python_under_roots(&self, as_path: &str) -> Option<String> {
        // Candidate roots: every distinct first-segment dir of known files.
        let roots: HashSet<&str> = self
            .known_files
            .iter()
            .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir))
            .flat_map(|dir| {
                // walk all ancestor dir prefixes
                let mut acc = Vec::new();
                let mut cur = dir;
                acc.push(cur);
                while let Some((parent, _)) = cur.rsplit_once('/') {
                    acc.push(parent);
                    cur = parent;
                }
                acc
            })
            .collect();
        for root in roots {
            let joined = format!("{}/{}", root, as_path);
            if let Some(t) = self.resolve_python_path(&joined) {
                return Some(t);
            }
        }
        None
    }

    // ---- Rust --------------------------------------------------------------

    fn resolve_rust(&self, from_file: &str, specifier: &str) -> Resolution {
        // `mod foo;` declaration edge — to_module is "mod foo".
        if let Some(name) = specifier.strip_prefix("mod ") {
            let name = name.trim();
            if let Some(t) = self.resolve_rust_mod(from_file, name) {
                return Resolution {
                    resolved_target: Some(t),
                    resolution: ResolutionKind::RustMod,
                };
            }
            return Resolution {
                resolved_target: None,
                resolution: ResolutionKind::Unresolved,
            };
        }

        // `use` path: to_module is e.g. "crate::foo", "super::bar", "self::baz",
        // "std::collections", "serde::Serialize".
        let segs: Vec<&str> = specifier.split("::").collect();
        let first = segs.first().copied().unwrap_or("");
        match first {
            "crate" => {
                // crate::foo -> resolve `foo` module from the crate root dir.
                if let Some(module) = segs.get(1) {
                    let crate_root = crate_root_dir(from_file);
                    if let Some(t) = self.resolve_rust_mod_in_dir(&crate_root, module) {
                        return Resolution {
                            resolved_target: Some(t),
                            resolution: ResolutionKind::RustMod,
                        };
                    }
                }
                Resolution {
                    resolved_target: None,
                    resolution: ResolutionKind::Unresolved,
                }
            }
            "self" => {
                if let Some(module) = segs.get(1) {
                    if let Some(t) = self.resolve_rust_mod(from_file, module) {
                        return Resolution {
                            resolved_target: Some(t),
                            resolution: ResolutionKind::RustMod,
                        };
                    }
                }
                Resolution {
                    resolved_target: None,
                    resolution: ResolutionKind::Unresolved,
                }
            }
            "super" => {
                // super::foo -> module `foo` in the parent module's directory.
                let parent = parent_dir(&module_dir_of(from_file));
                if let Some(module) = segs.get(1) {
                    if let Some(t) = self.resolve_rust_mod_in_dir(&parent, module) {
                        return Resolution {
                            resolved_target: Some(t),
                            resolution: ResolutionKind::RustMod,
                        };
                    }
                }
                Resolution {
                    resolved_target: None,
                    resolution: ResolutionKind::Unresolved,
                }
            }
            // Anything else is an extern crate (serde, tokio, std, ...) → External.
            _ => Resolution {
                resolved_target: None,
                resolution: ResolutionKind::External,
            },
        }
    }

    /// Resolve `mod foo;` declared in `from_file`: foo lives in the same module dir.
    fn resolve_rust_mod(&self, from_file: &str, name: &str) -> Option<String> {
        let dir = module_dir_of(from_file);
        self.resolve_rust_mod_in_dir(&dir, name)
    }

    /// `name` as a module under `dir`: `dir/name.rs` or `dir/name/mod.rs`.
    fn resolve_rust_mod_in_dir(&self, dir: &str, name: &str) -> Option<String> {
        let as_file = if dir.is_empty() {
            format!("{}.rs", name)
        } else {
            format!("{}/{}.rs", dir, name)
        };
        if self.known_files.contains(&as_file) {
            return Some(as_file);
        }
        let as_mod = if dir.is_empty() {
            format!("{}/mod.rs", name)
        } else {
            format!("{}/{}/mod.rs", dir, name)
        };
        if self.known_files.contains(&as_mod) {
            return Some(as_mod);
        }
        None
    }
}

// ============================================================================
// tsconfig discovery / parsing
// ============================================================================

fn discover_tsconfigs(graph: &CodeGraph, project_path: &Path) -> Vec<TsconfigScope> {
    // Find candidate directories: any dir containing a TS/JS file, plus the root.
    let mut dirs: HashSet<String> = HashSet::new();
    dirs.insert(String::new()); // repo root
    for f in &graph.files {
        if f.language == "typescript" || f.language == "javascript" {
            let mut d = parent_dir(&f.path);
            loop {
                dirs.insert(d.clone());
                if d.is_empty() {
                    break;
                }
                d = parent_dir(&d);
            }
        }
    }

    let mut scopes: Vec<TsconfigScope> = Vec::new();
    for dir in dirs {
        let tsconfig_rel = if dir.is_empty() {
            "tsconfig.json".to_string()
        } else {
            format!("{}/tsconfig.json", dir)
        };
        let full = project_path.join(&tsconfig_rel);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(scope) = parse_tsconfig(&dir, &content) {
            scopes.push(scope);
        }
    }
    // Most-specific (deepest) directory first so nested tsconfigs win.
    scopes.sort_by_key(|s| std::cmp::Reverse(s.dir.len()));
    scopes
}

fn parse_tsconfig(dir: &str, content: &str) -> Option<TsconfigScope> {
    // tsconfig.json permits comments/trailing commas; do a best-effort strip then parse.
    let cleaned = strip_jsonc(content);
    let val: serde_json::Value = serde_json::from_str(&cleaned).ok()?;
    let co = val.get("compilerOptions")?;

    let base_url = co
        .get("baseUrl")
        .and_then(|v| v.as_str())
        .map(|b| normalize_join(dir, b));

    let mut paths = Vec::new();
    if let Some(map) = co.get("paths").and_then(|p| p.as_object()) {
        for (pattern, subs) in map {
            let sub_list: Vec<String> = subs
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            paths.push((pattern.clone(), sub_list));
        }
    }

    if base_url.is_none() && paths.is_empty() {
        return None;
    }

    Some(TsconfigScope {
        dir: dir.to_string(),
        base_url,
        paths,
    })
}

/// Strip `//` and `/* */` comments and trailing commas from JSONC for serde_json.
fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if in_string {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < bytes.len() {
            let n = bytes[i + 1] as char;
            if n == '/' {
                // line comment
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if n == '*' {
                // block comment
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    // Remove trailing commas before } or ].
    remove_trailing_commas(&out)
}

fn remove_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (idx, &c) in chars.iter().enumerate() {
        if c == ',' {
            // peek next non-whitespace
            let mut j = idx + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                continue; // drop the comma
            }
        }
        out.push(c);
    }
    out
}

// ============================================================================
// Path / alias helpers (all operate on forward-slash repo-relative strings)
// ============================================================================

/// Match an alias pattern (e.g. `@/*`, `@app/*`, `utils`) against a specifier.
/// Returns the captured `*` portion (empty string for a non-wildcard exact match),
/// or `None` if no match.
fn match_alias(pattern: &str, specifier: &str) -> Option<String> {
    if let Some(prefix) = pattern.strip_suffix('*') {
        if specifier.starts_with(prefix) {
            return Some(specifier[prefix.len()..].to_string());
        }
        None
    } else if pattern == specifier {
        Some(String::new())
    } else {
        None
    }
}

/// Apply a captured wildcard portion to a substitution template (`src/*` + `foo` -> `src/foo`).
fn apply_alias(sub: &str, captured: &str) -> String {
    if let Some(prefix) = sub.strip_suffix('*') {
        format!("{}{}", prefix, captured)
    } else {
        sub.to_string()
    }
}

/// Whether `file` lives inside directory `dir` (dir == "" means repo root, matches all).
fn path_in_dir(file: &str, dir: &str) -> bool {
    if dir.is_empty() {
        return true;
    }
    file.starts_with(&format!("{}/", dir))
}

/// Parent directory of a repo-relative path (forward-slash). Root -> "".
fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

fn split_dir_file(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((dir, file)) => (dir.to_string(), file.to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// For Rust: the directory that hosts sibling modules of `from_file`.
/// `src/foo/mod.rs` and `src/lib.rs`/`src/main.rs` host modules in their OWN dir;
/// `src/foo.rs` hosts submodules in `src/foo/`.
fn module_dir_of(from_file: &str) -> String {
    let (dir, file) = split_dir_file(from_file);
    let stem = file.trim_end_matches(".rs");
    if stem == "mod" || stem == "lib" || stem == "main" {
        dir
    } else {
        // `foo.rs` declares submodules under `foo/`
        if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{}/{}", dir, stem)
        }
    }
}

/// The crate root directory (where `crate::` modules live): the dir containing
/// `src/lib.rs`/`src/main.rs`. Heuristic: the `src` dir prefix of `from_file`.
fn crate_root_dir(from_file: &str) -> String {
    // Find the last "src/" segment; crate modules live directly under that src dir.
    if let Some(idx) = from_file.rfind("src/") {
        return from_file[..idx + 3].to_string(); // include "src"
    }
    // Fallback: top-level dir.
    let (dir, _) = split_dir_file(from_file);
    let mut cur = dir.as_str();
    while let Some((parent, _)) = cur.rsplit_once('/') {
        cur = parent;
    }
    cur.to_string()
}

/// Strip a known source extension from a path, returning the stem-with-dirs.
fn strip_known_ext(path: &str) -> Option<String> {
    for ext in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "rs"] {
        let suffix = format!(".{}", ext);
        if let Some(stem) = path.strip_suffix(&suffix) {
            return Some(stem.to_string());
        }
    }
    None
}

/// Join a base directory with a relative-ish segment, collapsing `.`/`..` and
/// normalizing slashes. Both inputs are forward-slash repo-relative.
fn normalize_join(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Infer a language string from a file extension when not present in the graph map.
fn language_from_path(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "rs" => "rust",
        _ => "",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::code_graph::{CodeGraph, FileNode, ImportEdge};

    fn graph_with_files(paths: &[(&str, &str)]) -> CodeGraph {
        CodeGraph {
            files: paths
                .iter()
                .map(|(p, lang)| FileNode {
                    path: p.to_string(),
                    language: lang.to_string(),
                    line_count: 1,
                })
                .collect(),
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        }
    }

    #[test]
    fn ts_relative_with_extension_inference() {
        let g = graph_with_files(&[
            ("src/routes.ts", "typescript"),
            ("src/auth.ts", "typescript"),
        ]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/routes.ts", "./auth", "typescript");
        assert_eq!(res.resolved_target.as_deref(), Some("src/auth.ts"));
        assert_eq!(res.resolution, ResolutionKind::Relative);
    }

    #[test]
    fn ts_relative_parent_dir() {
        let g = graph_with_files(&[
            ("src/api/routes.ts", "typescript"),
            ("src/auth.ts", "typescript"),
        ]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/api/routes.ts", "../auth", "typescript");
        assert_eq!(res.resolved_target.as_deref(), Some("src/auth.ts"));
    }

    #[test]
    fn ts_directory_index() {
        let g = graph_with_files(&[
            ("src/app.ts", "typescript"),
            ("src/components/index.ts", "typescript"),
        ]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/app.ts", "./components", "typescript");
        assert_eq!(
            res.resolved_target.as_deref(),
            Some("src/components/index.ts")
        );
        assert_eq!(res.resolution, ResolutionKind::PackageIndex);
    }

    #[test]
    fn ts_bare_specifier_is_external() {
        let g = graph_with_files(&[("src/app.ts", "typescript")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/app.ts", "react", "typescript");
        assert_eq!(res.resolved_target, None);
        assert_eq!(res.resolution, ResolutionKind::External);
    }

    #[test]
    fn ts_broken_relative_is_unresolved() {
        let g = graph_with_files(&[("src/app.ts", "typescript")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/app.ts", "./does-not-exist", "typescript");
        assert_eq!(res.resolved_target, None);
        assert_eq!(res.resolution, ResolutionKind::Unresolved);
    }

    #[test]
    fn python_dotted_module() {
        let g = graph_with_files(&[("app/main.py", "python"), ("app/models.py", "python")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("app/main.py", "app.models", "python");
        assert_eq!(res.resolved_target.as_deref(), Some("app/models.py"));
        assert_eq!(res.resolution, ResolutionKind::PythonModule);
    }

    #[test]
    fn python_package_init() {
        let g = graph_with_files(&[("app/main.py", "python"), ("app/db/__init__.py", "python")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("app/main.py", "app.db", "python");
        assert_eq!(res.resolved_target.as_deref(), Some("app/db/__init__.py"));
    }

    #[test]
    fn python_relative_import() {
        let g = graph_with_files(&[
            ("app/api/routes.py", "python"),
            ("app/api/auth.py", "python"),
        ]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("app/api/routes.py", ".auth", "python");
        assert_eq!(res.resolved_target.as_deref(), Some("app/api/auth.py"));
        assert_eq!(res.resolution, ResolutionKind::PythonModule);
    }

    #[test]
    fn python_stdlib_is_external() {
        let g = graph_with_files(&[("app/main.py", "python")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("app/main.py", "os", "python");
        assert_eq!(res.resolution, ResolutionKind::External);
    }

    #[test]
    fn rust_mod_decl_to_sibling_file() {
        let g = graph_with_files(&[("src/lib.rs", "rust"), ("src/parser.rs", "rust")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/lib.rs", "mod parser", "rust");
        assert_eq!(res.resolved_target.as_deref(), Some("src/parser.rs"));
        assert_eq!(res.resolution, ResolutionKind::RustMod);
    }

    #[test]
    fn rust_mod_decl_to_mod_rs() {
        let g = graph_with_files(&[("src/lib.rs", "rust"), ("src/net/mod.rs", "rust")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/lib.rs", "mod net", "rust");
        assert_eq!(res.resolved_target.as_deref(), Some("src/net/mod.rs"));
    }

    #[test]
    fn rust_crate_use() {
        let g = graph_with_files(&[("src/main.rs", "rust"), ("src/config.rs", "rust")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/main.rs", "crate::config", "rust");
        assert_eq!(res.resolved_target.as_deref(), Some("src/config.rs"));
        assert_eq!(res.resolution, ResolutionKind::RustMod);
    }

    #[test]
    fn rust_extern_crate_is_external() {
        let g = graph_with_files(&[("src/main.rs", "rust")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/main.rs", "serde", "rust");
        assert_eq!(res.resolution, ResolutionKind::External);
    }

    #[test]
    fn rust_submodule_from_foo_rs() {
        // `foo.rs` declares `mod bar;` which lives in `foo/bar.rs`.
        let g = graph_with_files(&[("src/foo.rs", "rust"), ("src/foo/bar.rs", "rust")]);
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let res = r.resolve("src/foo.rs", "mod bar", "rust");
        assert_eq!(res.resolved_target.as_deref(), Some("src/foo/bar.rs"));
    }

    #[test]
    fn tsconfig_alias_resolution() {
        // Write a real tsconfig to a temp dir so discovery reads it.
        let tmp = std::env::temp_dir().join(format!("twinast-tsc-{}", std::process::id()));
        let _ = std::fs::create_dir_all(tmp.join("src/lib"));
        std::fs::write(
            tmp.join("tsconfig.json"),
            r#"{
  // baseUrl + paths alias
  "compilerOptions": {
    "baseUrl": ".",
    "paths": { "@/*": ["src/*"], }
  }
}"#,
        )
        .unwrap();

        let g = graph_with_files(&[
            ("src/app.ts", "typescript"),
            ("src/lib/utils.ts", "typescript"),
        ]);
        let r = ImportResolver::new(&g, &tmp);
        let res = r.resolve("src/app.ts", "@/lib/utils", "typescript");
        assert_eq!(res.resolved_target.as_deref(), Some("src/lib/utils.ts"));
        assert_eq!(res.resolution, ResolutionKind::TsconfigPath);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_graph_populates_edges() {
        let mut g = graph_with_files(&[
            ("src/routes.ts", "typescript"),
            ("src/auth.ts", "typescript"),
        ]);
        g.imports.push(ImportEdge {
            from_file: "src/routes.ts".into(),
            to_module: "./auth".into(),
            imported_names: vec!["authenticate".into()],
            line: 1,
            resolved_target: None,
            resolution: ResolutionKind::Unresolved,
        });
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        r.resolve_graph(&mut g);
        assert_eq!(g.imports[0].resolved_target.as_deref(), Some("src/auth.ts"));
        assert_eq!(g.imports[0].resolution, ResolutionKind::Relative);
    }
}
