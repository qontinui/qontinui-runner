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
//! module *structure* is built from the resolved `Ξ_AST` graph and summarized into
//! the prompt by [`super::module_graph`] (shared with `Ξ_Domain`); the model only
//! assigns layers — it never sees raw source.
//!
//! Per-module granularity is the vet's Q4 v1 decision. A process-local
//! fingerprint cache makes repeated calls on an unchanged graph free (the LLM pass
//! runs once per distinct graph fingerprint per process); durable app-data
//! persistence (§4.5) is a follow-up.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::{extract::State, routing::post, Json, Router};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};

use super::module_graph::{self, ModuleSummary};
use super::{code_graph_api, scope_project_dir, Envelope};
use crate::mcp::types::ApiState;

/// The architectural-layer taxonomy (UA's architecture-analyzer set + `unknown`).
const LAYERS: &[&str] = &["api", "service", "data", "ui", "utility", "unknown"];

/// Observer name + provenance for the kernel envelope.
const OBSERVER: &str = "arch";
const PROVENANCE: &str = super::PROV_KERNEL_ARCH;

// ===========================================================================
// Request / route
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
pub struct ArchLayersReq {
    /// Optional `(repo,language)` scope selector — a repo name/slug (cross-repo
    /// `Ξ_AST`), a project dir, or a tsconfig path.
    pub scope: Option<String>,
    /// Cap on modules classified in one pass (default
    /// [`module_graph::DEFAULT_MAX_MODULES`], clamped to 1..=200).
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
    let max_modules = req
        .max_modules
        .unwrap_or(module_graph::DEFAULT_MAX_MODULES)
        .clamp(1, 200);

    // Build the resolved graph + module summaries off the async runtime.
    let (summaries, total_modules, fingerprint) =
        match module_graph::build_module_graph(project_dir).await {
            Some(t) => t,
            None => return Json(cold_envelope(q, "graph build failed")),
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

    // The single LLM call (forced Claude-CLI, no API key); honest degrade on
    // failure → zero classified (coverage 0), never a confident false answer.
    let parsed = match module_graph::run_cli_structured(prompt).await {
        Some(output) => parse_layers(&output),
        None => Vec::new(),
    };

    cache_put(fingerprint, parsed.clone());
    Json(make_envelope(q, &scope.key, &parsed, total_modules, false))
}

// ===========================================================================
// Prompt construction (pure)
// ===========================================================================

/// Build the classification prompt: the arch-layer instruction header + the
/// shared module-list block. The model sees only structure (no source) and must
/// return strict JSON.
fn build_prompt(modules: &[ModuleSummary]) -> String {
    let mut s = String::from(
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
    s.push_str(&module_graph::render_modules(modules));
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
    let json_str = match module_graph::extract_json(output) {
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
    let posterior = if classified == 0 {
        0.0
    } else {
        module_graph::KERNEL_POSTERIOR
    };
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
        assert!((posterior - module_graph::KERNEL_POSTERIOR).abs() < 1e-9);
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
        assert!(
            cache_get(fp).is_none(),
            "empty classification is not cached"
        );
        let assignments = vec![LayerAssignment {
            module: "src/a".into(),
            layer: "api".into(),
            rationale: "x".into(),
        }];
        cache_put(fp, assignments.clone());
        assert_eq!(cache_get(fp), Some(assignments));
    }
}
