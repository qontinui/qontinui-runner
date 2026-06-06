//! `Ξ_Arch` — per-module architectural-layer observer (Phase 3 / Pillar 3 of the
//! twin-ast plan).
//!
//! A **stochastic-kernel** twin observer: it classifies each module (directory)
//! of a repo into an architectural layer (`api` / `service` / `data` / `ui` /
//! `utility` / `unknown`) by prompting the LLM through the runner's existing
//! `ai_provider` Claude-CLI path — **no API key, no `@anthropic-ai/sdk`** (the
//! binding operator constraint, satisfied by forcing the `claude_cli` provider).
//!
//! Unlike the deterministic `Ξ_AST` diff-impact surface, this observer is
//! **advisory and never gate-worthy**: its envelope carries `kernel:true`,
//! `posterior<1`, and `credibility=(causal:medium, authorial:low, boundary:low)`
//! — the model is producer-coupled (it read the same code) and authored nothing
//! independent. A consumer can tell "the parser resolved this import" (act on it)
//! from "the model thinks this is the Service layer" (a hint). This is exactly
//! the discipline UA (Understand-Anything) collapses by folding parser facts and
//! LLM guesses into one graph; the twin keeps them in distinct envelopes.
//!
//! Route: `POST /code-graph/arch-layers` — `{scope?, max_modules?}` → a uniform
//! kernel envelope wrapping `{modules:[{module,layer,rationale}], ...}`. The
//! module *structure* (files, exported symbols, resolved internal deps, external
//! packages) is built from the resolved `Ξ_AST` graph (Pillar 1) and summarized
//! into the prompt; the model only assigns layers — it never sees raw source.
//!
//! Per-module granularity is the vet's Q4 v1 decision (`Ξ_Domain` — business
//! flows — is deferred behind this). A process-local fingerprint cache makes
//! repeated calls on an unchanged graph free (the LLM pass runs once per distinct
//! graph fingerprint per process); durable app-data persistence (§4.5) is a
//! follow-up.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use axum::{extract::State, routing::post, Json, Router};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{code_graph_api, scope_project_dir, Envelope};
use crate::ai_provider::{run_structured_prompt, StructuredPrompt};
use crate::ai_router::TaskContext;
use crate::mcp::types::ApiState;
use crate::workflow_generation::code_graph::CodeGraph;

/// The architectural-layer taxonomy (UA's architecture-analyzer set + `unknown`).
const LAYERS: &[&str] = &["api", "service", "data", "ui", "utility", "unknown"];

/// Default cap on modules classified in one pass — bounds prompt size + token
/// cost. Modules beyond the cap (lowest export-count first) are omitted and
/// reflected honestly in `coverage`.
const DEFAULT_MAX_MODULES: usize = 60;

/// Uncalibrated kernel posterior (vet Q5): a fixed "model hint" confidence, NOT a
/// measured accuracy. `kernel:true` + `authorial:low` is what tells a consumer
/// this is advisory; the posterior is here to be calibrated against acceptance
/// data later, not trusted today.
const KERNEL_POSTERIOR: f64 = 0.5;

// Per-module summary caps (keep the prompt bounded regardless of repo size).
const MAX_EXPORTS_PER_MODULE: usize = 12;
const MAX_DEPS_PER_MODULE: usize = 8;
const MAX_EXTERNAL_PER_MODULE: usize = 8;
const MAX_FILES_PER_MODULE: usize = 10;

/// Observer name + provenance for the kernel envelope.
const OBSERVER: &str = "arch";
const PROVENANCE: &str = super::PROV_KERNEL_ARCH;

// ===========================================================================
// Request / route
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ArchLayersReq {
    /// Optional `(repo,language)` scope selector — a repo name/slug (cross-repo
    /// `Ξ_AST` via [`scope::repo_dir`]), a project dir, or a tsconfig path.
    pub scope: Option<String>,
    /// Cap on modules classified in one pass (default [`DEFAULT_MAX_MODULES`],
    /// clamped to 1..=200).
    pub max_modules: Option<usize>,
}

/// Routes contributed alongside the `/code-graph/*` surface.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/code-graph/arch-layers", post(arch_layers))
}

/// POST /code-graph/arch-layers → kernel envelope of per-module layer assignments.
async fn arch_layers(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ArchLayersReq>,
) -> Json<Envelope> {
    let q = "arch_layers";

    // Resolve the scope → project dir, reusing the diff-impact surface's resolver
    // (cross-repo repo-name → checkout, then path/tsconfig, then default).
    let scope = match code_graph_api::resolve_project_scope(req.scope.as_deref(), None) {
        Some(s) => s,
        None => return Json(cold_envelope(q, "no resolvable scope (cold)")),
    };
    let project_dir = scope_project_dir(&scope);
    let max_modules = req.max_modules.unwrap_or(DEFAULT_MAX_MODULES).clamp(1, 200);

    // Build the resolved graph + module summaries off the async runtime.
    let dir = project_dir.clone();
    let (summaries, total_modules, fingerprint) =
        match tokio::task::spawn_blocking(move || {
            let graph = CodeGraph::build(&dir);
            let mods = summarize_modules(&graph);
            let total = mods.len();
            let fp = fingerprint_modules(&mods);
            (mods, total, fp)
        })
        .await
        {
            Ok(t) => t,
            Err(_) => return Json(cold_envelope(q, "graph build failed")),
        };

    if total_modules == 0 {
        return Json(cold_envelope(q, "empty / cold graph (no modules)"));
    }

    // Fingerprint cache: a prior identical graph already classified → free.
    if let Some(cached) = cache_get(fingerprint) {
        return Json(make_envelope(q, &scope.key, &cached, total_modules, true));
    }

    // Cap to the highest-signal modules (most exports first); coverage reflects
    // anything omitted.
    let capped: Vec<ModuleSummary> = summaries.into_iter().take(max_modules).collect();
    let prompt = build_prompt(&capped);

    // The single LLM call — forced through the Claude-CLI provider so this
    // observer NEVER makes a keyed API call (the operator constraint). Synchronous
    // (spawns the CLI subprocess), so run it off the runtime.
    let response = match tokio::task::spawn_blocking(move || {
        let context = TaskContext::from_prompt(&prompt);
        let structured = StructuredPrompt::uncached(prompt);
        run_structured_prompt(
            &structured,
            &context,
            None,
            None,
            Some("claude_cli"), // provider_override — force CLI, no API key
            Some(0.0),          // temperature_override — deterministic classification
            None,
            None,
            None,
        )
    })
    .await
    {
        Ok(r) => r,
        Err(_) => {
            // The classification task panicked — degrade honestly, never 500.
            return Json(make_envelope(q, &scope.key, &[], total_modules, false));
        }
    };

    // Parse the model's assignments; an unavailable model / unparseable output
    // degrades to zero classified (coverage 0), never a confident false answer.
    let parsed = if response.success {
        parse_layers(&response.output)
    } else {
        Vec::new()
    };

    cache_put(fingerprint, parsed.clone());
    Json(make_envelope(q, &scope.key, &parsed, total_modules, false))
}

// ===========================================================================
// Module summarization (pure — built from the resolved Ξ_AST graph)
// ===========================================================================

/// A module = a directory of files, summarized for the classifier. The model
/// sees only this structure, never source.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModuleSummary {
    /// Module key — the directory path (repo-relative, forward slashes; `"."` for
    /// repo-root files).
    module: String,
    /// File basenames in the module (sorted, capped).
    files: Vec<String>,
    /// Exported symbol names declared in the module (sorted, capped).
    exports: Vec<String>,
    /// Other in-repo module dirs this module imports from, via *resolved* edges
    /// (sorted, capped).
    internal_deps: Vec<String>,
    /// External package specifiers the module imports (sorted, capped).
    external_pkgs: Vec<String>,
    /// Total exports before the cap — the ranking + tie-break signal.
    export_count: usize,
}

/// The directory component of a repo-relative path (forward slashes). Files at
/// the repo root map to `"."`.
fn module_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => ".".to_string(),
    }
}

/// The basename of a repo-relative path.
fn basename(path: &str) -> &str {
    path.rsplit_once('/').map(|(_, b)| b).unwrap_or(path)
}

/// Group the resolved graph's files into per-directory module summaries, sorted
/// by export count (desc), then module name (asc) for a stable, high-signal-first
/// ordering. Internal vectors are sorted + de-duplicated so the output (and its
/// fingerprint) is deterministic.
fn summarize_modules(graph: &CodeGraph) -> Vec<ModuleSummary> {
    // Accumulate per-module sets.
    struct Acc {
        files: BTreeSet<String>,
        exports: BTreeSet<String>,
        deps: BTreeSet<String>,
        external: BTreeSet<String>,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();
    let ensure = |acc: &mut BTreeMap<String, Acc>, m: String| {
        acc.entry(m).or_insert_with(|| Acc {
            files: BTreeSet::new(),
            exports: BTreeSet::new(),
            deps: BTreeSet::new(),
            external: BTreeSet::new(),
        });
    };

    for f in &graph.files {
        let m = module_dir(&f.path);
        ensure(&mut acc, m.clone());
        acc.get_mut(&m)
            .unwrap()
            .files
            .insert(basename(&f.path).to_string());
    }
    for e in &graph.exports {
        let m = module_dir(&e.file_path);
        ensure(&mut acc, m.clone());
        acc.get_mut(&m).unwrap().exports.insert(e.name.clone());
    }
    for imp in &graph.imports {
        let m = module_dir(&imp.from_file);
        ensure(&mut acc, m.clone());
        match &imp.resolved_target {
            Some(target) => {
                let target_mod = module_dir(target);
                if target_mod != m {
                    acc.get_mut(&m).unwrap().deps.insert(target_mod);
                }
            }
            None => {
                use crate::workflow_generation::code_graph::ResolutionKind;
                // Only count honestly-external specifiers as external packages;
                // an `Unresolved` internal-looking edge is a coverage hole, not a
                // package signal, so it is deliberately dropped here.
                if matches!(imp.resolution, ResolutionKind::External) {
                    acc.get_mut(&m)
                        .unwrap()
                        .external
                        .insert(imp.to_module.clone());
                }
            }
        }
    }

    let mut out: Vec<ModuleSummary> = acc
        .into_iter()
        .map(|(module, a)| {
            let export_count = a.exports.len();
            ModuleSummary {
                module,
                files: cap(a.files, MAX_FILES_PER_MODULE),
                exports: cap(a.exports, MAX_EXPORTS_PER_MODULE),
                internal_deps: cap(a.deps, MAX_DEPS_PER_MODULE),
                external_pkgs: cap(a.external, MAX_EXTERNAL_PER_MODULE),
                export_count,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.export_count
            .cmp(&a.export_count)
            .then_with(|| a.module.cmp(&b.module))
    });
    out
}

/// Take the first `n` of a sorted set into a `Vec` (the set is already ordered).
fn cap(set: BTreeSet<String>, n: usize) -> Vec<String> {
    set.into_iter().take(n).collect()
}

/// A stable fingerprint of the module structure — same structure → same value, so
/// the LLM pass runs once per distinct graph per process. Hashes a name-ordered
/// view of every module's (files, exports, internal deps); ignores the cap-driven
/// `export_count` ordering so only *content* changes invalidate the cache.
fn fingerprint_modules(mods: &[ModuleSummary]) -> u64 {
    let mut ordered: Vec<&ModuleSummary> = mods.iter().collect();
    ordered.sort_by(|a, b| a.module.cmp(&b.module));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for m in ordered {
        m.module.hash(&mut hasher);
        m.files.hash(&mut hasher);
        m.exports.hash(&mut hasher);
        m.internal_deps.hash(&mut hasher);
    }
    hasher.finish()
}

// ===========================================================================
// Prompt construction (pure)
// ===========================================================================

/// Build the classification prompt from the module summaries. The model sees only
/// structure (no source) and must return strict JSON. Kept deterministic (modules
/// in the given order, internal lists pre-sorted) so the cache + tests are stable.
fn build_prompt(modules: &[ModuleSummary]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are classifying the ARCHITECTURAL LAYER of each MODULE in a codebase.\n\
         A module is a directory. For each module you are given its files, its exported\n\
         symbols, the other in-repo modules it imports from, and the external packages\n\
         it uses. You do NOT see source code — classify from this structure alone.\n\n\
         Assign each module EXACTLY ONE layer from this taxonomy:\n\
         - api: HTTP/RPC route handlers, controllers, transport/request-response edges.\n\
         - service: business logic, orchestration, use-cases, domain operations.\n\
         - data: persistence, models, repositories, DB/ORM/migrations, schema.\n\
         - ui: user-interface components, views, widgets, frontend rendering.\n\
         - utility: cross-cutting helpers, shared types, config, logging, pure utils.\n\
         - unknown: the structure is insufficient to decide.\n\n\
         Return ONLY a JSON object, no prose, no code fences:\n\
         {\"modules\":[{\"module\":\"<exact module key>\",\"layer\":\"<taxonomy value>\",\"rationale\":\"<=12 words\"}]}\n\n\
         Modules:\n",
    );
    for (i, m) in modules.iter().enumerate() {
        s.push_str(&format!("{}. module: {}\n", i + 1, m.module));
        if !m.files.is_empty() {
            s.push_str(&format!("   files: {}\n", m.files.join(", ")));
        }
        if !m.exports.is_empty() {
            s.push_str(&format!("   exports: {}\n", m.exports.join(", ")));
        }
        if !m.internal_deps.is_empty() {
            s.push_str(&format!("   imports-from: {}\n", m.internal_deps.join(", ")));
        }
        if !m.external_pkgs.is_empty() {
            s.push_str(&format!("   external: {}\n", m.external_pkgs.join(", ")));
        }
    }
    s
}

// ===========================================================================
// Response parsing (pure, tolerant)
// ===========================================================================

/// One module's layer assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerAssignment {
    module: String,
    layer: String,
    rationale: String,
}

/// Parse `{"modules":[{module,layer,rationale}]}` (or a bare array) out of the
/// model's output, tolerant of surrounding prose / ```json fences. An unknown or
/// missing layer normalizes to `"unknown"`; entries without a `module` are
/// dropped. Returns an empty vec on any failure (the honest "I couldn't read it"
/// signal — the caller maps that to coverage 0, never a confident false answer).
fn parse_layers(output: &str) -> Vec<LayerAssignment> {
    let json_str = match extract_json(output) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    // Accept either `{"modules":[...]}` or a bare `[...]`.
    let arr = match &value {
        Value::Object(map) => map.get("modules").and_then(|v| v.as_array()).cloned(),
        Value::Array(a) => Some(a.clone()),
        _ => None,
    };
    let arr = match arr {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in arr {
        let module = entry
            .get("module")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());
        let module = match module {
            Some(m) if !m.is_empty() => m,
            _ => continue,
        };
        let layer = entry
            .get("layer")
            .and_then(|v| v.as_str())
            .map(normalize_layer)
            .unwrap_or_else(|| "unknown".to_string());
        let rationale = entry
            .get("rationale")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        out.push(LayerAssignment {
            module,
            layer,
            rationale,
        });
    }
    out
}

/// Normalize a model-supplied layer string to the taxonomy; anything off-taxonomy
/// (or empty) becomes `"unknown"` rather than leaking an invented label.
fn normalize_layer(raw: &str) -> String {
    let l = raw.trim().to_lowercase();
    if LAYERS.contains(&l.as_str()) {
        l
    } else {
        "unknown".to_string()
    }
}

/// Extract the first balanced JSON object/array from a string, ignoring prose and
/// ```json code fences and respecting string literals (so a `}` inside a string
/// doesn't close the scan early).
fn extract_json(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{' || b == b'[')?;
    let open = bytes[start];
    let close = if open == b'{' { b'}' } else { b']' };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            x if x == open => depth += 1,
            x if x == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

// ===========================================================================
// Envelope assembly
// ===========================================================================

/// Build the result body + coverage/posterior for a set of assignments over a
/// graph of `total_modules` modules. `coverage` = classified / total (honest
/// about cap-omitted + unparsed modules); `posterior` is the uncalibrated kernel
/// hint, or 0 when nothing was classified.
fn assemble(parsed: &[LayerAssignment], total_modules: usize, cached: bool) -> (Value, f64, f64) {
    let classified = parsed.len();
    let coverage = if total_modules == 0 {
        0.0
    } else {
        (classified as f64 / total_modules as f64).min(1.0)
    };
    let posterior = if classified == 0 { 0.0 } else { KERNEL_POSTERIOR };
    let modules: Vec<Value> = parsed
        .iter()
        .map(|a| json!({ "module": a.module, "layer": a.layer, "rationale": a.rationale }))
        .collect();
    let result = json!({
        "modules": modules,
        "total_modules": total_modules,
        "classified_modules": classified,
        "layer_taxonomy": LAYERS,
        "cached": cached,
    });
    (result, posterior, coverage)
}

fn make_envelope(
    query: &str,
    scope_key: &str,
    parsed: &[LayerAssignment],
    total_modules: usize,
    cached: bool,
) -> Envelope {
    let (mut result, posterior, coverage) = assemble(parsed, total_modules, cached);
    if let Value::Object(map) = &mut result {
        map.insert("scope".to_string(), json!(scope_key));
    }
    Envelope::kernel(query, OBSERVER, PROVENANCE, result, posterior, coverage)
}

/// An honest cold/degraded envelope: kernel, coverage 0, posterior 0, never an
/// assertion that the repo "has no layers".
fn cold_envelope(query: &str, reason: &str) -> Envelope {
    Envelope::kernel(
        query,
        OBSERVER,
        PROVENANCE,
        json!({
            "modules": [],
            "total_modules": 0,
            "classified_modules": 0,
            "layer_taxonomy": LAYERS,
            "reason": reason,
        }),
        0.0,
        0.0,
    )
}

// ===========================================================================
// Process-local fingerprint cache
// ===========================================================================

static CACHE: Lazy<Mutex<BTreeMap<u64, Vec<LayerAssignment>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

fn cache_get(fp: u64) -> Option<Vec<LayerAssignment>> {
    CACHE.lock().ok().and_then(|m| m.get(&fp).cloned())
}

fn cache_put(fp: u64, assignments: Vec<LayerAssignment>) {
    // Don't cache an empty (failed) classification — let the next call retry.
    if assignments.is_empty() {
        return;
    }
    if let Ok(mut m) = CACHE.lock() {
        m.insert(fp, assignments);
    }
}

// ===========================================================================
// Tests (pure — no LLM call)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_generation::code_graph::{
        ExportNode, FileNode, ImportEdge, ResolutionKind,
    };

    fn file(path: &str) -> FileNode {
        FileNode {
            path: path.into(),
            language: "typescript".into(),
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
    fn import(from: &str, to_module: &str, target: Option<&str>, kind: ResolutionKind) -> ImportEdge {
        ImportEdge {
            from_file: from.into(),
            to_module: to_module.into(),
            imported_names: vec![],
            line: 1,
            resolved_target: target.map(|s| s.into()),
            resolution: kind,
        }
    }

    #[test]
    fn module_dir_and_basename() {
        assert_eq!(module_dir("src/api/auth.ts"), "src/api");
        assert_eq!(module_dir("main.rs"), ".");
        assert_eq!(basename("src/api/auth.ts"), "auth.ts");
        assert_eq!(basename("main.rs"), "main.rs");
    }

    #[test]
    fn summarize_groups_files_exports_deps_and_external() {
        let graph = CodeGraph {
            files: vec![
                file("src/api/routes.ts"),
                file("src/api/handlers.ts"),
                file("src/services/auth.ts"),
            ],
            functions: vec![],
            classes: vec![],
            imports: vec![
                // api → services (resolved internal dep)
                import(
                    "src/api/routes.ts",
                    "../services/auth",
                    Some("src/services/auth.ts"),
                    ResolutionKind::Relative,
                ),
                // api → external package
                import("src/api/routes.ts", "express", None, ResolutionKind::External),
                // an unresolved internal-looking edge must NOT become an external pkg
                import("src/api/routes.ts", "./missing", None, ResolutionKind::Unresolved),
            ],
            exports: vec![
                export("registerRoutes", "src/api/routes.ts"),
                export("handle", "src/api/handlers.ts"),
                export("authenticate", "src/services/auth.ts"),
            ],
            build_duration_ms: 0,
        };
        let mods = summarize_modules(&graph);
        // Two modules: src/api (2 exports) ranks before src/services (1 export).
        assert_eq!(mods.len(), 2);
        assert_eq!(mods[0].module, "src/api");
        assert_eq!(mods[0].export_count, 2);
        assert!(mods[0].files.contains(&"routes.ts".to_string()));
        assert!(mods[0]
            .internal_deps
            .contains(&"src/services".to_string()));
        assert!(mods[0].external_pkgs.contains(&"express".to_string()));
        // The Unresolved edge is dropped from external packages (coverage hole, not a pkg).
        assert!(!mods[0].external_pkgs.contains(&"./missing".to_string()));
        assert_eq!(mods[1].module, "src/services");
    }

    #[test]
    fn fingerprint_is_stable_and_content_sensitive() {
        let g1 = CodeGraph {
            files: vec![file("src/a/x.ts")],
            functions: vec![],
            classes: vec![],
            imports: vec![],
            exports: vec![export("foo", "src/a/x.ts")],
            build_duration_ms: 0,
        };
        let g2 = g1.clone();
        let fp1 = fingerprint_modules(&summarize_modules(&g1));
        let fp2 = fingerprint_modules(&summarize_modules(&g2));
        assert_eq!(fp1, fp2, "identical structure → identical fingerprint");

        let mut g3 = g1.clone();
        g3.exports.push(export("bar", "src/a/x.ts"));
        let fp3 = fingerprint_modules(&summarize_modules(&g3));
        assert_ne!(fp1, fp3, "an added export must change the fingerprint");
    }

    #[test]
    fn prompt_contains_modules_taxonomy_and_json_instruction() {
        let mods = vec![ModuleSummary {
            module: "src/api".into(),
            files: vec!["routes.ts".into()],
            exports: vec!["registerRoutes".into()],
            internal_deps: vec!["src/services".into()],
            external_pkgs: vec!["express".into()],
            export_count: 1,
        }];
        let p = build_prompt(&mods);
        assert!(p.contains("src/api"));
        assert!(p.contains("registerRoutes"));
        assert!(p.contains("imports-from: src/services"));
        assert!(p.contains("external: express"));
        // taxonomy + strict-JSON instruction present
        assert!(p.contains("service:"));
        assert!(p.contains("\"modules\""));
    }

    #[test]
    fn parse_clean_json_object() {
        let out = r#"{"modules":[{"module":"src/api","layer":"api","rationale":"route handlers"},{"module":"src/db","layer":"data","rationale":"models"}]}"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].module, "src/api");
        assert_eq!(parsed[0].layer, "api");
        assert_eq!(parsed[1].layer, "data");
    }

    #[test]
    fn parse_tolerates_prose_and_code_fences() {
        let out = "Sure — here is the classification:\n```json\n{\"modules\":[{\"module\":\"src/ui\",\"layer\":\"UI\",\"rationale\":\"views\"}]}\n```\nLet me know if you need more.";
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].module, "src/ui");
        // layer is normalized to lowercase taxonomy.
        assert_eq!(parsed[0].layer, "ui");
    }

    #[test]
    fn parse_normalizes_offtaxonomy_layer_to_unknown() {
        let out = r#"{"modules":[{"module":"src/x","layer":"controller","rationale":"?"}]}"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].layer, "unknown");
    }

    #[test]
    fn parse_accepts_bare_array_and_drops_module_less_entries() {
        let out = r#"[{"module":"src/a","layer":"service"},{"layer":"data"}]"#;
        let parsed = parse_layers(out);
        assert_eq!(parsed.len(), 1, "entry without a module is dropped");
        assert_eq!(parsed[0].module, "src/a");
        // missing rationale defaults to empty.
        assert_eq!(parsed[0].rationale, "");
    }

    #[test]
    fn parse_garbage_returns_empty() {
        assert!(parse_layers("I cannot help with that.").is_empty());
        assert!(parse_layers("").is_empty());
        assert!(parse_layers("{not json").is_empty());
    }

    #[test]
    fn extract_json_respects_string_literals() {
        // A `}` inside a string must not close the object early.
        let s = "prefix {\"k\":\"a}b\",\"n\":1} suffix";
        assert_eq!(extract_json(s), Some("{\"k\":\"a}b\",\"n\":1}"));
    }

    #[test]
    fn assemble_coverage_and_kernel_discipline() {
        let parsed = vec![
            LayerAssignment {
                module: "src/a".into(),
                layer: "api".into(),
                rationale: "x".into(),
            },
            LayerAssignment {
                module: "src/b".into(),
                layer: "service".into(),
                rationale: "y".into(),
            },
        ];
        // 2 classified of 4 total → coverage 0.5, posterior = kernel hint.
        let (result, posterior, coverage) = assemble(&parsed, 4, false);
        assert!((coverage - 0.5).abs() < 1e-9);
        assert!((posterior - KERNEL_POSTERIOR).abs() < 1e-9);
        assert_eq!(result["classified_modules"], json!(2));
        assert_eq!(result["total_modules"], json!(4));

        // Nothing classified → coverage 0, posterior 0 (honest, never confident).
        let (_r, p0, c0) = assemble(&[], 4, false);
        assert_eq!(p0, 0.0);
        assert_eq!(c0, 0.0);
    }

    #[test]
    fn envelope_is_advisory_kernel_low_authorial() {
        let parsed = vec![LayerAssignment {
            module: "src/a".into(),
            layer: "api".into(),
            rationale: "x".into(),
        }];
        let env = make_envelope("arch_layers", "scope-key", &parsed, 1, false);
        assert!(env.kernel, "Ξ_Arch envelope MUST be kernel:true");
        assert!(env.posterior < 1.0, "kernel posterior must be < 1");
        assert_eq!(env.observer, OBSERVER);
        assert_eq!(env.provenance, PROVENANCE);
        // The credibility discipline: producer-coupled, authored nothing.
        assert_eq!(env.credibility.authorial, "low");
        assert_eq!(env.credibility.boundary, "low");
        assert_eq!(env.credibility.causal, "medium");
        assert_eq!(env.result["scope"], json!("scope-key"));
    }

    #[test]
    fn cold_envelope_does_not_assert_absence() {
        let env = cold_envelope("arch_layers", "empty / cold graph (no modules)");
        assert!(env.kernel);
        assert_eq!(env.coverage, 0.0);
        assert_eq!(env.posterior, 0.0);
        assert_eq!(env.result["classified_modules"], json!(0));
        // Honest cold result carries a reason, not a false "no layers" claim.
        assert!(env.result.get("reason").is_some());
    }

    #[test]
    fn cache_skips_empty_and_roundtrips_nonempty() {
        // A distinctive fingerprint unlikely to collide with other tests.
        let fp = 0xA5A5_DEAD_BEEF_1234u64;
        cache_put(fp, Vec::new());
        assert!(cache_get(fp).is_none(), "empty classification is not cached");
        let assignments = vec![LayerAssignment {
            module: "src/a".into(),
            layer: "api".into(),
            rationale: "x".into(),
        }];
        cache_put(fp, assignments.clone());
        assert_eq!(cache_get(fp), Some(assignments));
    }
}
