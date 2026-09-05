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
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;

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
    let graph = spawn_blocking_tracked(move || CodeGraph::build(&dir))
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
    let (resolution, coverage) = spawn_blocking_tracked(move || {
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
        // `Some(_)` is AUTHORITATIVE, INCLUDING an empty list. `Some(vec![])`
        // means "declarations changed and every one of them survived" - positive
        // knowledge that nothing was removed. Treating it as `None` (which this
        // arm used to do via `if !removed.is_empty()`) sends a retype-only patch
        // down the conservative path below, where the observer flags the retyped
        // symbol AND every sibling export of its file - strictly worse than the
        // false positive being fixed.
        Some(removed) => removed
            .iter()
            .filter(|r| changed_set.contains(r.file.as_str()))
            // HEAD CONFIRMATION, free here and unavailable to coord: this graph
            // was built from the LIVE working tree, so `graph.exports` IS the
            // post-change export set. A name still exported by the file was not
            // removed, whatever the patch's `-` lines looked like.
            .filter(|r| {
                !graph
                    .exports
                    .iter()
                    .any(|e| e.file_path == r.file && e.name == r.name)
            })
            .map(|r| (r.file.clone(), r.name.clone()))
            .collect(),
        // No patch at all (or the parser recognized nothing) -> every export of
        // a changed file is at-risk. NOT confirmable against the graph: these
        // candidates come FROM `graph.exports`, so the filter above would empty
        // the set.
        None => graph
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

/// Parse the exports a unified diff removes, as a per-file `-` minus `+` set
/// difference.
///
/// **BOTH SIDES of the diff are read.** A declaration line only counts as a
/// removal when the same `(file, class, name)` is not also declared on the `+`
/// side — otherwise every retype / re-signature / reformat of a still-exported
/// symbol reads as a deletion, because a unified diff represents an in-place
/// edit as a `-`/`+` pair and a one-sided parser only ever sees the `-` half.
///
/// This mirrors coord's `code_graph_service::parse_removed_exports_detailed`,
/// which is the authority for the algorithm; coord's copy gates merges, this one
/// is advisory (it feeds the `coord_change_conflict` observer). Keep them in
/// step — a divergent twin teaches the next reader the wrong algorithm.
///
/// Subtraction is per `(file, name)`, not patch-wide:
///
/// - Removing `X` and adding a different `Y` in the same file still flags `X`.
/// - A rename `OLD` → `NEW` still flags `OLD`; `NEW` was never removed.
/// - `pub fn foo` → `fn foo` still flags `foo` — only *exported* forms are
///   recognized, so the private `+` side yields no name to subtract. Dropping
///   `pub` IS a removal from the public surface.
/// - A move (removed from A, added in B) still flags it against A.
/// - `export const Foo` → `export type Foo` still flags `Foo`: different
///   [`DeclClass`]es, and a name surviving as a type-only export does not keep
///   runtime importers working.
///
/// ACCEPTED FALSE NEGATIVE — the recognizer is scope-blind, so moving
/// `pub fn new` between `impl` blocks in one file, or turning a module-level
/// `def process` into a method, cancels and is not flagged. Requiring equal
/// indentation would close it and resurrect reformatting false positives; the
/// failure mode of record here is over-flagging.
///
/// Return is THREE-state:
/// - `None` — no `-` line parsed as a declaration at all; the caller cannot tell
///   "nothing removed" from "parser blind to this language", so it falls back to
///   the all-exports-at-risk policy.
/// - `Some(vec![])` — declarations changed and every one survived. Positive
///   knowledge, and it must NOT be collapsed into `None`: doing so sends a
///   retype-only patch down the conservative path, where the observer flags the
///   retyped symbol *and every sibling export of its file*.
/// - `Some(list)` — exactly these `(file, name)` were removed. Deduped.
pub fn parse_removed_exports_from_patch(patch: &str) -> Option<Vec<RemovedExport>> {
    use std::collections::{HashMap, HashSet};

    let mut current_file: Option<String> = None;
    // The `--- a/<path>` seen on the IMMEDIATELY preceding line. A `--- ` line
    // only counts as a header when a `+++ ` follows it; requiring adjacency
    // keeps a deleted SQL comment (`-- x`, reaching us as `--- x`) from posing
    // as a file header.
    let mut pending_old: Option<String> = None;
    let mut added: HashMap<String, HashSet<(DeclClass, String)>> = HashMap::new();
    let mut removed: Vec<(DeclClass, RemovedExport)> = Vec::new();
    // Files whose export surface cannot be determined from the patch — see
    // [`LineDecls::Opaque`]. Never reported on.
    let mut unresolvable: HashSet<String> = HashSet::new();
    let mut matched_any_removal = false;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            let f = strip_diff_prefix(rest.trim());
            pending_old = (f != "/dev/null" && !f.is_empty()).then_some(f);
            continue;
        }
        let old_side = pending_old.take();

        if let Some(rest) = line.strip_prefix("+++ ") {
            let f = strip_diff_prefix(rest.trim());
            current_file = if f != "/dev/null" && !f.is_empty() {
                Some(f)
            } else {
                // Whole-file DELETE: the new side is `/dev/null`, so attribute
                // this hunk's removals to the path being deleted. Keeping the
                // PREVIOUS file's path would mis-attribute them and let that
                // unrelated file's `+` side cancel them.
                old_side
            };
            continue;
        }

        if let Some(body) = line.strip_prefix('-') {
            if let Some(file) = &current_file {
                match export_decls_in_line(body) {
                    LineDecls::Opaque => {
                        matched_any_removal = true;
                        unresolvable.insert(file.clone());
                    }
                    LineDecls::Decls(decls) => {
                        for (class, name) in decls {
                            matched_any_removal = true;
                            removed.push((
                                class,
                                RemovedExport {
                                    file: file.clone(),
                                    name,
                                },
                            ));
                        }
                    }
                    LineDecls::None => {}
                }
            }
        } else if let Some(body) = line.strip_prefix('+') {
            if let Some(file) = &current_file {
                match export_decls_in_line(body) {
                    LineDecls::Opaque => {
                        unresolvable.insert(file.clone());
                    }
                    LineDecls::Decls(decls) => {
                        added.entry(file.clone()).or_default().extend(decls);
                    }
                    LineDecls::None => {}
                }
            }
        } else if let Some(body) = line.strip_prefix(' ') {
            // CONTEXT line, consulted only for the opaque marker: an untouched
            // `export *` still makes the file's surface unknowable. Names are
            // NOT collected — an unchanged declaration is neither added nor
            // removed, and treating it as added would let context cancel a
            // genuine removal in the same hunk.
            if let Some(file) = &current_file {
                if matches!(export_decls_in_line(body), LineDecls::Opaque) {
                    unresolvable.insert(file.clone());
                }
            }
        }
    }

    removed.retain(|(class, r)| {
        !added
            .get(&r.file)
            .is_some_and(|decls| decls.contains(&(*class, r.name.clone())))
    });
    // A file whose export surface is indeterminate is dropped entirely, even for
    // names that parsed cleanly: "this name has no `+`-side twin" proves nothing
    // in a file that may re-export it through a form this parser cannot read.
    removed.retain(|(_, r)| !unresolvable.contains(&r.file));

    let mut seen: HashSet<(String, String)> = HashSet::new();
    let out: Vec<RemovedExport> = removed
        .into_iter()
        .map(|(_, r)| r)
        .filter(|r| seen.insert((r.file.clone(), r.name.clone())))
        .collect();

    if out.is_empty() && !matched_any_removal {
        None
    } else {
        Some(out)
    }
}

/// What a single source line contributes to a file's export surface.
///
/// The third arm is the one that matters. A line-based parser can read
/// `export { A, B as C } from "./x"` exactly, but it cannot read
/// `export * from "./x"` at all — resolving that to names means following the
/// target module, which is not in the patch. Reporting such a file's removals
/// anyway would report a symbol set known to be incomplete.
enum LineDecls {
    /// Nothing on this line contributes an exported name.
    None,
    /// Exactly these `(class, name)` pairs are exported by this line. A
    /// re-export list declares several, which is why this is a `Vec`.
    Decls(Vec<(DeclClass, String)>),
    /// The line changes the export surface in a way whose name set cannot be
    /// determined from the line alone — a star re-export, or a braced list that
    /// does not close on this line.
    Opaque,
}

/// What kind of export a declaration contributes, for deciding whether a `-`/`+`
/// pair with the same name is the SAME export.
///
/// Only TypeScript needs the distinction: `type`/`interface` are erased at
/// runtime, so `export const Foo` becoming `export type Foo` keeps the name
/// while destroying the value export. Rust and Python erase nothing, so all
/// their forms share [`DeclClass::Value`] and freely cancel each other.
///
/// Deliberately coarser than the keyword: `export const` → `export function`
/// (the everyday arrow-fn refactor) must still cancel, so keying on the exact
/// keyword would resurrect the false positive this exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DeclClass {
    /// Survives to runtime.
    Value,
    /// Erased by the TypeScript compiler: `export type` / `export interface`.
    TypeOnly,
}

/// Extract every exported/declared symbol a single source line contributes,
/// across TS/Python/Rust declaration forms and the TS re-export forms.
///
/// | Form | Result |
/// |---|---|
/// | `export { A, B as C };` | `[A, C]` — the exported name is the ALIAS |
/// | `export type { T, U } from "./x";` | `[T, U]`, both type-only |
/// | `export { type A, B };` | `A` type-only, `B` value |
/// | `export * as ns from "./x";` | `[ns]` |
/// | `export * from "./x";` | [`LineDecls::Opaque`] |
fn export_decls_in_line(line: &str) -> LineDecls {
    use DeclClass::{TypeOnly, Value};

    let one = |class: DeclClass, after: &str| match ident(after) {
        Some(n) => LineDecls::Decls(vec![(class, n)]),
        None => LineDecls::None,
    };

    let t = line.trim_start();

    // TS: export [default] [async] function|class|const|let|var|interface|type|enum NAME
    if let Some(rest) = t.strip_prefix("export ") {
        let rest = rest.trim_start();

        if let Some(after_star) = rest.strip_prefix('*') {
            let after_star = after_star.trim_start();
            return match after_star.strip_prefix("as ") {
                Some(alias) => match ident(alias.trim_start()) {
                    Some(n) => LineDecls::Decls(vec![(Value, n)]),
                    None => LineDecls::Opaque,
                },
                None => LineDecls::Opaque,
            };
        }

        // NOTE: `rest` is deliberately NOT rebound — `export type Foo = Bar`
        // must still reach the keyword loop below.
        let (list_body, list_class) = match rest.strip_prefix("type ") {
            Some(after) => (after.trim_start(), TypeOnly),
            None => (rest, Value),
        };
        if let Some(after_brace) = list_body.strip_prefix('{') {
            return match after_brace.find('}') {
                Some(end) => {
                    LineDecls::Decls(export_specifier_list(&after_brace[..end], list_class))
                }
                None => LineDecls::Opaque,
            };
        }

        let rest = rest
            .trim_start_matches("default ")
            .trim_start_matches("async ");
        for (kw, class) in [
            ("function ", Value),
            ("class ", Value),
            ("const ", Value),
            ("let ", Value),
            ("var ", Value),
            ("interface ", TypeOnly),
            ("type ", TypeOnly),
            ("enum ", Value),
        ] {
            if let Some(after) = rest.strip_prefix(kw) {
                return one(class, after);
            }
        }
        return LineDecls::None;
    }
    // Python: top-level `def NAME` / `class NAME` (module-public).
    for kw in ["def ", "class ", "async def "] {
        if let Some(after) = t.strip_prefix(kw) {
            return one(Value, after);
        }
    }
    // Rust: pub fn|struct|enum|trait|const|static|type|mod NAME
    if let Some(rest) = t.strip_prefix("pub ") {
        let rest = rest.trim_start_matches("(crate) ").trim_start();
        for kw in [
            "fn ", "struct ", "enum ", "trait ", "const ", "static ", "type ", "mod ",
        ] {
            if let Some(after) = rest.strip_prefix(kw) {
                return one(Value, after);
            }
        }
    }
    LineDecls::None
}

/// Split the inside of an `export { … }` list into the names it EXPORTS.
///
/// `default_class` is the list's own class (`TypeOnly` for
/// `export type { … }`); an inline `type ` modifier on a single specifier
/// overrides it for that specifier only.
fn export_specifier_list(body: &str, default_class: DeclClass) -> Vec<(DeclClass, String)> {
    let mut out = Vec::new();
    for raw in body.split(',') {
        let item = raw.trim();
        if item.is_empty() {
            // Trailing comma, or an empty list.
            continue;
        }
        let (item, class) = match item.strip_prefix("type ") {
            Some(after) => (after.trim(), DeclClass::TypeOnly),
            None => (item, default_class),
        };
        // `A as B` exports `B`; `A as default` exports `default`. The exported
        // name is always the right-hand side.
        let token = match item.split(" as ").nth(1) {
            Some(alias) => alias.trim(),
            None => item,
        };
        if let Some(n) = ident(token) {
            out.push((class, n));
        }
    }
    out
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
///
/// `pub(super)` so sibling observers on the same sidecar (e.g. `arch_observer`)
/// reuse the one scope resolver rather than forking a second abstraction (vet Q7).
pub(super) fn resolve_project_scope(
    scope: Option<&str>,
    hint_file: Option<&String>,
) -> Option<Scope> {
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
            // `src/auth.ts` no longer exports `authenticate` — this graph is
            // built from the LIVE working tree, which is the post-change state,
            // and the patch below removes it. The dangling `src/api.ts` import
            // edge above is what makes this a Contradiction.
            //
            // This fixture used to list the export here while also claiming the
            // patch removed it, which is a state that cannot occur: the tree
            // cannot both have and not have the symbol. It went unnoticed while
            // nothing cross-checked the two, and the head confirmation in
            // `compute_removed_exports_referenced` now does.
            exports: vec![],
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

    // =======================================================================
    // Ported from coord PR #1520 + the re-export extension. This parser is the
    // ADVISORY twin of coord's merge gate; it carried the one-sided defect
    // verbatim until this change, so these cases exist to keep the two in step.
    // =======================================================================

    /// The defect this port exists for: a unified diff renders an in-place edit
    /// as a `-`/`+` pair, and a parser that reads only the `-` half calls every
    /// retype a deletion.
    #[test]
    fn retype_is_not_a_removal() {
        let patch = "\
--- a/src/hooks/useToast.ts
+++ b/src/hooks/useToast.ts
@@ -10,1 +10,1 @@
-export type ShowToastFn = (message: string, type: ToastType) => void;
+export type ShowToastFn = (message: string, type: ToastType, action?: ToastAction) => void;
";
        let removed = parse_removed_exports_from_patch(patch).expect("a declaration changed");
        assert!(
            removed.is_empty(),
            "a re-declared export is not a removal, got {removed:?}"
        );
    }

    /// The counter-test, deliberately adjacent: if a future change makes the
    /// case above pass by weakening the parser, this one fails.
    #[test]
    fn genuine_removal_is_still_reported() {
        let patch = "\
--- a/src/hooks/useToast.ts
+++ b/src/hooks/useToast.ts
@@ -19,1 +19,0 @@
-export type ShowToastFn = (message: string, type: ToastType) => void;
";
        let removed = parse_removed_exports_from_patch(patch).expect("removed");
        assert_eq!(removed.len(), 1, "got {removed:?}");
        assert_eq!(removed[0].name, "ShowToastFn");
    }

    /// Privatization IS a removal from the public surface — the `+` side is not
    /// an exported form, so it yields no name to subtract. This is what stops
    /// anyone "fixing" the retype case with a substring check.
    #[test]
    fn privatization_is_a_removal() {
        let patch = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,1 +1,1 @@
-pub fn helper() {}
+fn helper() {}
";
        let removed = parse_removed_exports_from_patch(patch).expect("pub -> private is a removal");
        assert_eq!(removed.len(), 1, "got {removed:?}");
        assert_eq!(removed[0].name, "helper");
    }

    /// A rename flags the OLD name only; the new one was never removed.
    #[test]
    fn rename_flags_only_the_old_name() {
        let patch = "\
--- a/src/auth.ts
+++ b/src/auth.ts
@@ -1,1 +1,1 @@
-export function authenticate() {}
+export function authorize() {}
";
        let removed = parse_removed_exports_from_patch(patch).expect("old name is removed");
        assert_eq!(removed.len(), 1, "got {removed:?}");
        assert_eq!(removed[0].name, "authenticate");
    }

    /// `export { A, B as C }` — the EXPORTED name is the alias, so dropping
    /// `B as C` removes `C`. The old recognizer returned `None` for both sides
    /// of this diff, making barrel modules invisible in both directions.
    #[test]
    fn reexport_list_drop_flags_the_alias() {
        let patch = "\
--- a/src/index.ts
+++ b/src/index.ts
@@ -1,1 +1,1 @@
-export { A, B as C } from \"./x\";
+export { A } from \"./x\";
";
        let removed = parse_removed_exports_from_patch(patch).expect("C is removed");
        assert_eq!(removed.len(), 1, "got {removed:?}");
        assert_eq!(removed[0].name, "C");
    }

    /// A bare `export * from` cannot be resolved to names without following the
    /// target module, so the FILE is dropped from the candidate set — including
    /// names on other lines that did parse cleanly.
    #[test]
    fn bare_star_reexport_makes_the_file_unresolvable() {
        let patch = "\
--- a/src/index.ts
+++ b/src/index.ts
@@ -1,2 +1,1 @@
-export * from \"./x\";
-export { Kept } from \"./y\";
";
        let removed = parse_removed_exports_from_patch(patch).expect("declarations changed");
        assert!(
            removed.is_empty(),
            "an unresolvable file yields no candidates, got {removed:?}"
        );
    }

    /// ...and a star sitting in the hunk's CONTEXT counts too: the removed name
    /// may survive through a re-export the patch never touched.
    #[test]
    fn star_reexport_in_context_also_makes_the_file_unresolvable() {
        let patch = "\
--- a/src/index.ts
+++ b/src/index.ts
@@ -1,3 +1,2 @@
 export * from \"./internals\";
-export { A } from \"./internals\";
 export { B } from \"./other\";
";
        let removed = parse_removed_exports_from_patch(patch).expect("declarations changed");
        assert!(removed.is_empty(), "got {removed:?}");
    }

    /// A context line must never CANCEL a removal — it is neither added nor
    /// removed.
    #[test]
    fn context_lines_do_not_cancel_a_removal() {
        let patch = "\
--- a/src/auth.ts
+++ b/src/auth.ts
@@ -1,3 +1,2 @@
 export const authenticate = () => {};
-export const authorize = () => {};
 export const audit = () => {};
";
        let removed = parse_removed_exports_from_patch(patch).expect("authorize is removed");
        assert_eq!(removed.len(), 1, "got {removed:?}");
        assert_eq!(removed[0].name, "authorize");
    }

    /// The three-state contract: `Some(vec![])` is POSITIVE knowledge that
    /// nothing was removed and must not collapse into `None`. Collapsing them
    /// sends a retype-only patch down the conservative all-exports path, where
    /// the observer flags the retyped symbol AND every sibling export of its
    /// file — strictly worse than the false positive being fixed.
    #[test]
    fn empty_some_does_not_fall_back_to_all_exports() {
        let graph = CodeGraph {
            files: vec![file("src/auth.ts", "typescript")],
            functions: vec![],
            classes: vec![],
            imports: vec![import(
                "src/api.ts",
                "src/auth.ts",
                &["sibling"],
                ResolutionKind::Relative,
            )],
            exports: vec![
                export("authenticate", "src/auth.ts"),
                export("sibling", "src/auth.ts"),
            ],
            build_duration_ms: 0,
        };
        // A retype-only patch: parsed, and everything survived.
        let parsed = Some(vec![]);
        let removed =
            compute_removed_exports_referenced(&graph, &["src/auth.ts".to_string()], &parsed);
        assert!(
            removed.is_empty(),
            "an empty Some must flag nothing, got {removed:?}"
        );

        // `None` (no patch at all) still takes the conservative path.
        let conservative =
            compute_removed_exports_referenced(&graph, &["src/auth.ts".to_string()], &None);
        assert_eq!(
            conservative.len(),
            1,
            "None must still flag the referenced sibling"
        );
    }

    /// HEAD CONFIRMATION — free in the runner, where the graph IS the live
    /// working tree. A candidate the patch called removed, but which the tree
    /// still exports, is not reported. coord cannot do this without
    /// materializing a second tree.
    #[test]
    fn candidate_still_exported_by_the_live_tree_is_dropped() {
        let graph = CodeGraph {
            files: vec![file("src/auth.ts", "typescript")],
            functions: vec![],
            classes: vec![],
            imports: vec![import(
                "src/api.ts",
                "src/auth.ts",
                &["authenticate"],
                ResolutionKind::Relative,
            )],
            // The symbol is STILL exported at head.
            exports: vec![export("authenticate", "src/auth.ts")],
            build_duration_ms: 0,
        };
        let claimed_removed = Some(vec![RemovedExport {
            file: "src/auth.ts".to_string(),
            name: "authenticate".to_string(),
        }]);
        let removed = compute_removed_exports_referenced(
            &graph,
            &["src/auth.ts".to_string()],
            &claimed_removed,
        );
        assert!(
            removed.is_empty(),
            "the tree still exports it, so it was not removed; got {removed:?}"
        );
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
