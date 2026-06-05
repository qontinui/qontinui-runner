//! Graph-level diff blast-radius `predict-effect` surface — the `Ξ_AST` sibling
//! of the LSP `Ξ_Type` sidecar (Pillar 2 of the twin-ast plan).
//!
//! Two HTTP routes, co-located with the code-semantics sidecar (same axum router,
//! same `$QONTINUI_LSP_SIDECAR_URL` host process). Both return the **uniform
//! observation envelope** (CONTRACT §C) via [`super::Envelope::resolved`]:
//!
//! - `POST /code-graph/diff-impact` — given `{scope?, changed_files}` **or**
//!   `{scope?, diff_patch}`, build the resolved `Ξ_AST` graph for the scope's
//!   project, compute the [`BlastRadius`], and additionally surface
//!   `removed_exports_referenced` (exports of changed files that are still
//!   referenced by a *resolved* import edge elsewhere). A non-empty
//!   `removed_exports_referenced` maps to `outcome_signal:"Contradiction"`
//!   (the §D active-negation signal); otherwise `Confirmed`/`Partial` per coverage.
//! - `POST /code-graph/resolve-import` — `{scope?, from_file, specifier}` →
//!   `{resolved_target, resolution}` via the Phase 1 [`ImportResolver::resolve`]
//!   single-specifier path (debug/introspection; no per-language logic duplicated).
//!
//! **Scope** reuses the sidecar's `(repo, language)` model: [`scope::resolve_scope`]
//! → [`super::scope_project_dir`] gives the project root, exactly as
//! `/code-semantics/symbol-lookup` resolves it. v1 = single default scope (a
//! degenerate case of the registry), no second scope abstraction.
//!
//! **Coverage honesty (CONTRACT §B/D4):** the resolved graph's coverage is the
//! fraction of *internal-looking* edges that bound to a file — an `Unresolved`
//! edge lowers coverage below 1. A cold/empty graph yields `coverage<1`, so the
//! tool never emits a false "no impact" on a partial graph.

use std::path::Path;
use std::sync::Arc;

use axum::{extract::State, routing::post, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use super::scope::{self, Scope};
use super::{scope_project_dir, Envelope};
use crate::mcp::types::ApiState;
use crate::workflow_generation::code_graph::{BlastRadius, CodeGraph, ResolutionKind};
use crate::workflow_generation::import_resolver::ImportResolver;

// ===========================================================================
// Request bodies (plan §3.1)
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
pub struct DiffImpactReq {
    /// Optional `(repo,language)` scope selector (project dir or tsconfig path).
    pub scope: Option<String>,
    /// Repo-relative changed file paths. Mutually inclusive with `diff_patch`
    /// (paths parsed from the patch are merged in).
    #[serde(default)]
    pub changed_files: Vec<String>,
    /// A unified diff; changed file paths (and removed exports) are parsed from it.
    pub diff_patch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveImportReq {
    pub scope: Option<String>,
    pub from_file: String,
    pub specifier: String,
}

// ===========================================================================
// Routes
// ===========================================================================

/// Routes contributed alongside the `/code-semantics/*` surface (merged from the
/// same `code_semantics::routes()` in `mcp_api.rs`).
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/code-graph/diff-impact", post(diff_impact))
        .route("/code-graph/resolve-import", post(resolve_import))
}

/// POST /code-graph/diff-impact → uniform envelope wrapping `BlastRadius` +
/// `removed_exports_referenced` + `outcome_signal`.
async fn diff_impact(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<DiffImpactReq>,
) -> Json<Envelope> {
    let q = "diff_impact";

    // Resolve the scope → project dir, reusing the sidecar's scope model.
    let scope = match resolve_project_scope(req.scope.as_deref(), req.changed_files.first()) {
        Some(s) => s,
        None => {
            // No resolvable scope at all → honest cold envelope (coverage 0),
            // never a "no impact" assertion.
            return Json(Envelope::resolved(
                q,
                json!({
                    "blast_radius": empty_blast_value(),
                    "removed_exports_referenced": [],
                    "outcome_signal": "Partial",
                    "reason": "no resolvable scope (cold)",
                }),
                0.0,
            ));
        }
    };
    let project_dir = scope_project_dir(&scope);

    // Merge explicitly-listed changed files with paths parsed out of the patch.
    let removed_from_patch = req
        .diff_patch
        .as_deref()
        .map(parse_removed_exports_from_patch)
        .unwrap_or_default();
    let mut changed: Vec<String> = req.changed_files.clone();
    if let Some(patch) = req.diff_patch.as_deref() {
        for f in parse_changed_files_from_patch(patch) {
            if !changed.contains(&f) {
                changed.push(f);
            }
        }
    }

    // Build the resolved graph (CPU-bound parse) off the async runtime.
    let dir = project_dir.clone();
    let graph = tokio::task::spawn_blocking(move || CodeGraph::build(&dir))
        .await
        .unwrap_or_else(|_| empty_graph());

    let coverage = graph_coverage(&graph);
    let br = graph.blast_radius(&changed);

    // Whether the patch named specific removed exports; if it didn't (or only
    // `changed_files` was given), treat ALL exports of changed files as at-risk.
    let removed = compute_removed_exports_referenced(&graph, &changed, &removed_from_patch);

    // Outcome signal (CONTRACT §D): a removed export still referenced elsewhere is
    // the repo-level Contradiction. Otherwise Confirmed (full coverage) / Partial.
    let outcome_signal = if !removed.is_empty() {
        "Contradiction"
    } else if coverage >= 1.0 {
        "Confirmed"
    } else {
        "Partial"
    };

    let result = json!({
        "scope": scope.key,
        "changed_files": changed,
        "blast_radius": blast_to_value(&br),
        "removed_exports_referenced": removed,
        "outcome_signal": outcome_signal,
        // Document the at-risk-export policy used (per plan §3.1).
        "removed_exports_policy": if removed_from_patch.is_some() {
            "patch_removed_exports"
        } else {
            "all_changed_file_exports_at_risk"
        },
    });

    Json(Envelope::resolved(q, result, coverage))
}

/// POST /code-graph/resolve-import → `{resolved_target, resolution}` via the
/// Phase 1 single-specifier `ImportResolver::resolve` (no rule duplication).
async fn resolve_import(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ResolveImportReq>,
) -> Json<Envelope> {
    let q = "resolve_import";

    let scope = match resolve_project_scope(req.scope.as_deref(), Some(&req.from_file)) {
        Some(s) => s,
        None => {
            return Json(Envelope::resolved(
                q,
                json!({ "resolved_target": Value::Null, "resolution": "unresolved",
                        "reason": "no resolvable scope (cold)" }),
                0.0,
            ));
        }
    };
    let project_dir = scope_project_dir(&scope);

    let from_file = req.from_file.clone();
    let specifier = req.specifier.clone();
    let dir = project_dir.clone();
    let (resolution, coverage) = tokio::task::spawn_blocking(move || {
        let graph = CodeGraph::build(&dir);
        let cov = graph_coverage(&graph);
        let resolver = ImportResolver::new(&graph, &dir);
        let lang = language_of(&graph, &from_file);
        (resolver.resolve(&from_file, &specifier, &lang), cov)
    })
    .await
    .unwrap_or((
        crate::workflow_generation::import_resolver::Resolution {
            resolved_target: None,
            resolution: ResolutionKind::Unresolved,
        },
        0.0,
    ));

    let result = json!({
        "scope": scope.key,
        "from_file": req.from_file,
        "specifier": req.specifier,
        "resolved_target": resolution.resolved_target,
        "resolution": resolution_str(resolution.resolution),
    });
    Json(Envelope::resolved(q, result, coverage))
}

// ===========================================================================
// removed_exports_referenced + coverage
// ===========================================================================

/// Compute `removed_exports_referenced`: for each export of a changed file that
/// DISAPPEARS, the resolved reference edges (an `ImportEdge` whose
/// `resolved_target` points at the changed file AND whose `imported_names`
/// include the export) from *other* files. A non-empty result is the
/// Contradiction signal (a removed export still referenced elsewhere).
///
/// `removed_from_patch`: if `Some`, the explicit set of `(file, export_name)`
/// the patch removes — only those are considered. If `None` (only `changed_files`
/// given, no patch), ALL exports of changed files are treated as at-risk (the
/// conservative policy documented in plan §3.1).
fn compute_removed_exports_referenced(
    graph: &CodeGraph,
    changed_files: &[String],
    removed_from_patch: &Option<Vec<RemovedExport>>,
) -> Vec<Value> {
    use std::collections::HashSet;
    let changed_set: HashSet<&str> = changed_files.iter().map(|s| s.as_str()).collect();

    // Candidate (file, export_name) pairs that are "removed / at-risk".
    let candidates: Vec<(String, String)> = match removed_from_patch {
        Some(removed) if !removed.is_empty() => removed
            .iter()
            .filter(|r| changed_set.contains(r.file.as_str()))
            .map(|r| (r.file.clone(), r.name.clone()))
            .collect(),
        // No explicit patch removals → every export of a changed file is at-risk.
        _ => graph
            .exports
            .iter()
            .filter(|e| changed_set.contains(e.file_path.as_str()))
            .map(|e| (e.file_path.clone(), e.name.clone()))
            .collect(),
    };

    let mut out = Vec::new();
    for (file, name) in candidates {
        // Resolved reference edges: another file whose resolved_target is the
        // changed file and that imports this exact name.
        let referenced_by: Vec<Value> = graph
            .imports
            .iter()
            .filter(|imp| {
                imp.from_file != file
                    && imp.resolved_target.as_deref() == Some(file.as_str())
                    && imp.imported_names.iter().any(|n| n == &name)
            })
            .map(|imp| json!({ "file": imp.from_file, "line": imp.line }))
            .collect();

        if !referenced_by.is_empty() {
            out.push(json!({
                "name": name,
                "file": file,
                "referenced_by": referenced_by,
            }));
        }
    }
    out
}

/// Coverage = fraction of *internal-looking* import edges the resolver bound to a
/// file. `External` edges are excluded (a third-party specifier is honestly
/// external, not a coverage hole); `Unresolved` edges lower coverage. An empty /
/// cold graph (no files) → coverage 0 so the tool never asserts a false "no
/// impact" on a partial graph.
fn graph_coverage(graph: &CodeGraph) -> f64 {
    if graph.files.is_empty() {
        return 0.0;
    }
    let internal: Vec<&ResolutionKind> = graph
        .imports
        .iter()
        .map(|i| &i.resolution)
        .filter(|k| !matches!(k, ResolutionKind::External))
        .collect();
    if internal.is_empty() {
        // No internal edges to resolve — the graph is fully "covered" for the
        // purpose of dependency fan-out (warm, files present).
        return 1.0;
    }
    let bound = internal
        .iter()
        .filter(|k| !matches!(k, ResolutionKind::Unresolved))
        .count();
    bound as f64 / internal.len() as f64
}

// ===========================================================================
// Unified-diff parsing
// ===========================================================================

/// Parse the set of changed file paths out of a unified diff. Recognizes
/// `diff --git a/<path> b/<path>`, `+++ b/<path>`, and `--- a/<path>` headers;
/// strips the `a/`/`b/` prefix and normalizes slashes.
pub fn parse_changed_files_from_patch(patch: &str) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    let mut push = |p: String| {
        if !p.is_empty() && p != "/dev/null" && !files.contains(&p) {
            files.push(p);
        }
    };
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            push(strip_diff_prefix(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("--- ") {
            push(strip_diff_prefix(rest.trim()));
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            // `a/foo b/foo`
            for tok in rest.split_whitespace() {
                push(strip_diff_prefix(tok));
            }
        }
    }
    files
}

/// A `(file, export_name)` the patch removes (a deleted `export` line).
#[derive(Debug, Clone)]
pub struct RemovedExport {
    pub file: String,
    pub name: String,
}

/// Parse removed exports from a unified diff: a `-` line (removal) that declares
/// an export. Tracks the current file via the `+++ b/<path>` header. Recognizes
/// TS (`export function|class|const|let|var|interface|type|enum NAME`), Python
/// (`def NAME` / `class NAME`), and Rust (`pub fn|struct|enum|trait|const NAME`).
/// Returns `None` if the patch contains no removed-export lines (so the caller
/// falls back to the all-exports-at-risk policy).
pub fn parse_removed_exports_from_patch(patch: &str) -> Option<Vec<RemovedExport>> {
    let mut current_file: Option<String> = None;
    let mut removed: Vec<RemovedExport> = Vec::new();

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let f = strip_diff_prefix(rest.trim());
            if f != "/dev/null" && !f.is_empty() {
                current_file = Some(f);
            }
            continue;
        }
        // A removed line (single leading '-', not the '--- ' header).
        if let Some(body) = line.strip_prefix('-') {
            if body.starts_with("-- ") || line.starts_with("--- ") {
                continue;
            }
            if let Some(file) = &current_file {
                if let Some(name) = export_name_in_line(body) {
                    removed.push(RemovedExport {
                        file: file.clone(),
                        name,
                    });
                }
            }
        }
    }

    if removed.is_empty() {
        None
    } else {
        Some(removed)
    }
}

/// Extract an exported/declared symbol name from a single source line, across
/// TS/Python/Rust declaration forms. Returns the identifier, or `None`.
fn export_name_in_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    // TS: export [default] [async] function|class|const|let|var|interface|type|enum NAME
    if let Some(rest) = t.strip_prefix("export ") {
        let rest = rest
            .trim_start_matches("default ")
            .trim_start_matches("async ");
        for kw in [
            "function ",
            "class ",
            "const ",
            "let ",
            "var ",
            "interface ",
            "type ",
            "enum ",
        ] {
            if let Some(after) = rest.strip_prefix(kw) {
                return ident(after);
            }
        }
    }
    // Python: top-level `def NAME` / `class NAME` (module-public).
    for kw in ["def ", "class ", "async def "] {
        if let Some(after) = t.strip_prefix(kw) {
            return ident(after);
        }
    }
    // Rust: pub fn|struct|enum|trait|const|static NAME
    if let Some(rest) = t.strip_prefix("pub ") {
        let rest = rest.trim_start_matches("(crate) ").trim_start();
        for kw in [
            "fn ", "struct ", "enum ", "trait ", "const ", "static ", "type ", "mod ",
        ] {
            if let Some(after) = rest.strip_prefix(kw) {
                return ident(after);
            }
        }
    }
    None
}

/// First identifier token (`[A-Za-z_][A-Za-z0-9_]*`) at the start of `s`.
fn ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let name: String = s
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() || name.chars().next().map(|c| c.is_numeric()).unwrap_or(true) {
        None
    } else {
        Some(name)
    }
}

/// Strip a `a/` or `b/` git-diff prefix and any trailing tab-metadata, normalize slashes.
fn strip_diff_prefix(s: &str) -> String {
    // Drop trailing "\t<timestamp>" metadata some diffs carry.
    let s = s.split('\t').next().unwrap_or(s).trim();
    let s = s
        .strip_prefix("a/")
        .or_else(|| s.strip_prefix("b/"))
        .unwrap_or(s);
    s.replace('\\', "/")
}

// ===========================================================================
// Scope / helpers
// ===========================================================================

/// Resolve a scope to a project directory for the multi-language `Ξ_AST` graph.
/// An explicit `scope` selector is checked FIRST — as a repo name (cross-repo
/// registry), then a tsconfig path, then an existing dir — so a non-TS repo
/// (coord/Rust, web/Python) resolves to its OWN root instead of silently falling
/// back to the runner's default TS frontend (the prior behavior, which made the
/// surface single-repo). Only when no explicit selector resolves do we fall back
/// to the file hint's nearest tsconfig and then the default scope.
fn resolve_project_scope(scope: Option<&str>, hint_file: Option<&String>) -> Option<Scope> {
    if let Some(s) = scope {
        // (a) Repo NAME / `owner/name` slug → local checkout dir (cross-repo
        //     Ξ_AST; CodeGraph::build walks TS/JS/Python/Rust under it).
        if let Some(dir) = scope::repo_dir(s) {
            return Some(raw_dir_scope(&dir));
        }
        let p = Path::new(s);
        // (b) Explicit tsconfig.json file → TS scope (LSP-compatible).
        if p.is_file() && p.file_name().map(|n| n == "tsconfig.json").unwrap_or(false) {
            return Some(Scope::ts(p));
        }
        // (c) Explicit existing dir → TS scope if it has a tsconfig, else a raw
        //     multi-language project root.
        if p.is_dir() {
            let tsconfig = p.join("tsconfig.json");
            if tsconfig.exists() {
                return Some(Scope::ts(&tsconfig));
            }
            return Some(raw_dir_scope(p));
        }
        // An explicit selector that is neither a known repo nor a path falls
        // through to the file-hint / default resolution below.
    }
    // No explicit scope resolved: the file hint's nearest tsconfig, then default.
    scope::resolve_scope(None, hint_file.map(|f| f.as_str()))
}

/// A degenerate scope whose `project` descriptor is a raw directory (no tsconfig).
/// `scope_project_dir` takes the parent of `project`, so we append a synthetic
/// `tsconfig.json` segment to make the parent equal the real dir.
fn raw_dir_scope(dir: &Path) -> Scope {
    let project = scope::normalize(&dir.join("tsconfig.json"));
    Scope {
        key: scope::normalize(dir),
        language: "mixed".to_string(),
        project,
    }
}

/// The language string for `from_file`, taken from the graph (or inferred from
/// the extension), so `ImportResolver::resolve` dispatches the right rules.
fn language_of(graph: &CodeGraph, from_file: &str) -> String {
    if let Some(f) = graph.files.iter().find(|f| f.path == from_file) {
        return f.language.clone();
    }
    match from_file.rsplit('.').next().unwrap_or("") {
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "rs" => "rust",
        _ => "",
    }
    .to_string()
}

fn resolution_str(k: ResolutionKind) -> &'static str {
    match k {
        ResolutionKind::Relative => "relative",
        ResolutionKind::TsconfigPath => "tsconfig_path",
        ResolutionKind::PackageIndex => "package_index",
        ResolutionKind::PythonModule => "python_module",
        ResolutionKind::RustMod => "rust_mod",
        ResolutionKind::External => "external",
        ResolutionKind::Unresolved => "unresolved",
    }
}

fn blast_to_value(br: &BlastRadius) -> Value {
    json!({
        "directly_affected": br.directly_affected,
        "transitively_affected": br.transitively_affected,
        "affected_exports": br.affected_exports,
        "risk_level": format!("{:?}", br.risk_level).to_lowercase(),
        "total_impact_count": br.total_impact_count,
    })
}

fn empty_blast_value() -> Value {
    json!({
        "directly_affected": [],
        "transitively_affected": [],
        "affected_exports": [],
        "risk_level": "low",
        "total_impact_count": 0,
    })
}

fn empty_graph() -> CodeGraph {
    CodeGraph {
        files: vec![],
        functions: vec![],
        classes: vec![],
        imports: vec![],
        exports: vec![],
        build_duration_ms: 0,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::code_graph::{ExportNode, FileNode, ImportEdge};

    fn file(path: &str, lang: &str) -> FileNode {
        FileNode {
            path: path.into(),
            language: lang.into(),
            line_count: 10,
        }
    }

    fn export(name: &str, file: &str) -> ExportNode {
        ExportNode {
            name: name.into(),
            file_path: file.into(),
            kind: "function".into(),
            line: 1,
        }
    }

    fn import(from: &str, target: &str, names: &[&str], kind: ResolutionKind) -> ImportEdge {
        ImportEdge {
            from_file: from.into(),
            to_module: "./x".into(),
            imported_names: names.iter().map(|s| s.to_string()).collect(),
            line: 3,
            resolved_target: Some(target.into()),
            resolution: kind,
        }
    }

    /// (a) A diff deleting an exported symbol still imported elsewhere yields a
    /// non-empty `removed_exports_referenced` with the `referenced_by` list —
    /// the Contradiction signal.
    #[test]
    fn removed_export_still_referenced_is_contradiction() {
        let graph = CodeGraph {
            files: vec![
                file("src/auth.ts", "typescript"),
                file("src/api.ts", "typescript"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![import(
                "src/api.ts",
                "src/auth.ts",
                &["authenticate"],
                ResolutionKind::Relative,
            )],
            exports: vec![export("authenticate", "src/auth.ts")],
            build_duration_ms: 0,
        };
        // Patch removes `export function authenticate` from src/auth.ts.
        let patch = "+++ b/src/auth.ts\n-export function authenticate() {}\n";
        let removed_from_patch = parse_removed_exports_from_patch(patch);
        assert!(removed_from_patch.is_some());

        let removed = compute_removed_exports_referenced(
            &graph,
            &["src/auth.ts".to_string()],
            &removed_from_patch,
        );
        assert_eq!(removed.len(), 1, "the removed export is still referenced");
        let entry = &removed[0];
        assert_eq!(entry["name"], json!("authenticate"));
        assert_eq!(entry["file"], json!("src/auth.ts"));
        let refs = entry["referenced_by"].as_array().unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0]["file"], json!("src/api.ts"));
        assert_eq!(refs[0]["line"], json!(3));

        // outcome_signal computed from a non-empty removed set is Contradiction.
        assert!(!removed.is_empty());
    }

    /// A removed export that is NOT referenced anywhere → empty (no contradiction).
    #[test]
    fn removed_export_not_referenced_is_clear() {
        let graph = CodeGraph {
            files: vec![file("src/auth.ts", "typescript")],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![export("authenticate", "src/auth.ts")],
            build_duration_ms: 0,
        };
        let removed =
            compute_removed_exports_referenced(&graph, &["src/auth.ts".to_string()], &None);
        assert!(removed.is_empty());
    }

    /// (b) A cold / partial graph yields coverage < 1 (never a false "no impact").
    #[test]
    fn cold_graph_coverage_below_one() {
        assert_eq!(graph_coverage(&empty_graph()), 0.0);

        // A graph with one Unresolved internal edge → coverage 0.5.
        let graph = CodeGraph {
            files: vec![
                file("src/a.ts", "typescript"),
                file("src/b.ts", "typescript"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![
                import("src/a.ts", "src/b.ts", &["x"], ResolutionKind::Relative),
                ImportEdge {
                    from_file: "src/a.ts".into(),
                    to_module: "./missing".into(),
                    imported_names: vec!["y".into()],
                    line: 2,
                    resolved_target: None,
                    resolution: ResolutionKind::Unresolved,
                },
            ],
            exports: vec![],
            build_duration_ms: 0,
        };
        let cov = graph_coverage(&graph);
        assert!(cov < 1.0, "an unresolved internal edge must lower coverage");
        assert!((cov - 0.5).abs() < 1e-9, "1 of 2 internal edges bound");
    }

    /// External-only edges don't count as coverage holes.
    #[test]
    fn external_edges_do_not_lower_coverage() {
        let graph = CodeGraph {
            files: vec![file("src/a.ts", "typescript")],
            functions: vec![],
            classes: vec![],
            imports: vec![ImportEdge {
                from_file: "src/a.ts".into(),
                to_module: "react".into(),
                imported_names: vec!["useState".into()],
                line: 1,
                resolved_target: None,
                resolution: ResolutionKind::External,
            }],
            exports: vec![],
            build_duration_ms: 0,
        };
        assert_eq!(graph_coverage(&graph), 1.0);
    }

    #[test]
    fn parse_changed_files_from_unified_diff() {
        let patch = "\
diff --git a/src/auth.ts b/src/auth.ts
index 111..222 100644
--- a/src/auth.ts
+++ b/src/auth.ts
@@ -1,3 +1,2 @@
-export function authenticate() {}
diff --git a/src/new.ts b/src/new.ts
new file mode 100644
--- /dev/null
+++ b/src/new.ts
";
        let files = parse_changed_files_from_patch(patch);
        assert!(files.contains(&"src/auth.ts".to_string()));
        assert!(files.contains(&"src/new.ts".to_string()));
        assert!(!files.contains(&"/dev/null".to_string()));
    }

    #[test]
    fn parse_removed_exports_across_languages() {
        let patch = "\
+++ b/src/a.ts
-export function fooTs() {}
+++ b/pkg/b.py
-def foo_py():
+++ b/src/c.rs
-pub fn foo_rs() {}
";
        let removed = parse_removed_exports_from_patch(patch).unwrap();
        let names: Vec<&str> = removed.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"fooTs"));
        assert!(names.contains(&"foo_py"));
        assert!(names.contains(&"foo_rs"));
        // file attribution follows the +++ header.
        assert_eq!(
            removed.iter().find(|r| r.name == "foo_py").unwrap().file,
            "pkg/b.py"
        );
    }

    /// (c) resolve-import returns the right `resolved_target`/`resolution` for a
    /// relative, an alias, and an external specifier (exercises the Phase 1
    /// single-specifier path the route reuses).
    #[test]
    fn resolve_import_relative_alias_external() {
        // relative
        let g = CodeGraph {
            files: vec![
                file("src/routes.ts", "typescript"),
                file("src/auth.ts", "typescript"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        let r = ImportResolver::new(&g, Path::new("/nonexistent"));
        let rel = r.resolve("src/routes.ts", "./auth", "typescript");
        assert_eq!(rel.resolved_target.as_deref(), Some("src/auth.ts"));
        assert_eq!(resolution_str(rel.resolution), "relative");

        // external
        let ext = r.resolve("src/routes.ts", "react", "typescript");
        assert_eq!(ext.resolved_target, None);
        assert_eq!(resolution_str(ext.resolution), "external");

        // alias (needs a real tsconfig on disk)
        let tmp = std::env::temp_dir().join(format!("twinast-cgapi-{}", std::process::id()));
        let _ = std::fs::create_dir_all(tmp.join("src/lib"));
        std::fs::write(
            tmp.join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
        )
        .unwrap();
        let g2 = CodeGraph {
            files: vec![
                file("src/app.ts", "typescript"),
                file("src/lib/utils.ts", "typescript"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![],
            build_duration_ms: 0,
        };
        let r2 = ImportResolver::new(&g2, &tmp);
        let alias = r2.resolve("src/app.ts", "@/lib/utils", "typescript");
        assert_eq!(alias.resolved_target.as_deref(), Some("src/lib/utils.ts"));
        assert_eq!(resolution_str(alias.resolution), "tsconfig_path");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn raw_dir_scope_parents_to_real_dir() {
        let s = raw_dir_scope(Path::new("/some/project"));
        let dir = scope_project_dir(&s);
        assert_eq!(scope::normalize(&dir), "/some/project");
    }

    #[test]
    fn explicit_dir_without_tsconfig_resolves_to_raw_multilang_root() {
        // The core cross-repo fix: an explicit existing dir with NO tsconfig must
        // resolve to a raw multi-language project root AT that dir — not silently
        // fall back to the runner's default TS frontend (the prior single-repo bug).
        let tmp = std::env::temp_dir().join("qontinui_xrepo_api_test_root");
        std::fs::create_dir_all(&tmp).unwrap();
        let s = resolve_project_scope(Some(tmp.to_str().unwrap()), None).expect("scope resolves");
        assert_eq!(s.language, "mixed");
        assert_eq!(s.key, scope::normalize(&tmp));
        // scope_project_dir of a raw scope returns the real dir (synthetic tsconfig parent).
        assert_eq!(scope_project_dir(&s), tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unknown_selector_falls_through_to_default_scope() {
        // A selector that is neither a known repo nor an existing path falls
        // through to the default scope (the runner frontend tsconfig), never erroring.
        let s = resolve_project_scope(Some("not-a-real-repo-or-path-xyz"), None);
        assert!(s.is_some(), "should fall back to default TS scope");
        assert!(s.unwrap().key.ends_with("tsconfig.json"));
    }
}
