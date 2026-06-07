//! `Ξ_Domain` — business-domain / flow mapping observer (Phase 3 / Pillar 3 of the
//! twin-ast plan; the fuzzier sibling sequenced behind `Ξ_Arch` per vet Q4).
//!
//! A **stochastic-kernel** twin observer that maps a repo's modules onto the
//! business/functional **domains** (and the flows) they implement — UA's
//! domain-analyzer (`/understand-domain`). Where `Ξ_Arch` answers "what *layer* is
//! this module" against a fixed taxonomy, `Ξ_Domain` *discovers* an open-ended set
//! of domains (Authentication, Billing, Notifications, …) and groups modules under
//! them. It runs through the same Claude-CLI path — **no API key** — and shares
//! the module-graph summarization, JSON extraction, and forced-CLI call with
//! `Ξ_Arch` via [`super::module_graph`].
//!
//! Same kernel discipline as `Ξ_Arch`: **advisory, never gate-worthy** —
//! `kernel:true`, `posterior<1`, `credibility=(causal:medium, authorial:low,
//! boundary:low)`. The model sees only module structure, never source.
//!
//! Route: `POST /code-graph/domains` — `{scope?, max_modules?}` → a uniform kernel
//! envelope wrapping `{domains:[{name,summary,modules,flows}], ...}`.
//!
//! **Coverage is honest about hallucination.** Each domain's `modules` is
//! validated against the real module-key set; a module the model invents (renamed
//! / nonexistent) is dropped from the output and does NOT count toward coverage.
//! `coverage` = distinct *real* modules assigned to some domain / total modules
//! (so cap-omitted, unassigned, and hallucinated modules all lower it honestly).
//!
//! v1 ships the pure observer only; the `Ξ_Domain`-vs-resolved-dependency-graph
//! declared-vs-actual / Φ layering-breach drift pair is Phase 4 (needs the
//! declared-vs-actual construct doc + a declared-side source).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use axum::{extract::State, routing::post, Json, Router};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};

use super::module_graph::{self, ModuleSummary};
use super::{code_graph_api, scope_project_dir, Envelope};
use crate::mcp::types::ApiState;

/// Observer name + provenance for the kernel envelope.
const OBSERVER: &str = "domain";
const PROVENANCE: &str = super::PROV_KERNEL_DOMAIN;

// ===========================================================================
// Request / route
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
pub struct DomainsReq {
    /// Optional `(repo,language)` scope selector — a repo name/slug (cross-repo
    /// `Ξ_AST`), a project dir, or a tsconfig path.
    pub scope: Option<String>,
    /// Cap on modules sent to the model in one pass (default
    /// [`module_graph::DEFAULT_MAX_MODULES`], clamped to 1..=200).
    pub max_modules: Option<usize>,
}

/// Routes contributed alongside the `/code-graph/*` surface.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/code-graph/domains", post(domains))
}

/// POST /code-graph/domains → kernel envelope of discovered business domains.
async fn domains(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<DomainsReq>,
) -> Json<Envelope> {
    let q = "domains";

    let scope = match code_graph_api::resolve_project_scope(req.scope.as_deref(), None) {
        Some(s) => s,
        None => return Json(cold_envelope(q, "no resolvable scope (cold)")),
    };
    let project_dir = scope_project_dir(&scope);
    let max_modules = req
        .max_modules
        .unwrap_or(module_graph::DEFAULT_MAX_MODULES)
        .clamp(1, 200);

    let (summaries, total_modules, fingerprint) =
        match module_graph::build_module_graph(project_dir).await {
            Some(t) => t,
            None => return Json(cold_envelope(q, "graph build failed")),
        };

    if total_modules == 0 {
        return Json(cold_envelope(q, "empty / cold graph (no modules)"));
    }

    // The real module-key set — used to validate the model's groupings (drop
    // hallucinated module paths, compute honest coverage). Built from ALL modules,
    // not just the capped subset sent to the model.
    let module_keys: BTreeSet<String> = summaries.iter().map(|m| m.module.clone()).collect();

    if let Some(cached) = cache_get(fingerprint) {
        return Json(make_envelope(
            q,
            &scope.key,
            &cached,
            total_modules,
            &module_keys,
            true,
        ));
    }

    let capped: Vec<ModuleSummary> = summaries.into_iter().take(max_modules).collect();
    let prompt = build_prompt(&capped);

    let parsed = match module_graph::run_cli_structured(prompt).await {
        Some(output) => parse_domains(&output),
        None => Vec::new(),
    };

    cache_put(fingerprint, parsed.clone());
    Json(make_envelope(
        q,
        &scope.key,
        &parsed,
        total_modules,
        &module_keys,
        false,
    ))
}

// ===========================================================================
// Prompt construction (pure)
// ===========================================================================

/// Build the domain-mapping prompt: the instruction header + the shared
/// module-list block. The model sees only structure (no source) and must return
/// strict JSON.
fn build_prompt(modules: &[ModuleSummary]) -> String {
    let mut s = String::from(
        "You are mapping a codebase to its BUSINESS DOMAINS and flows.\n\
         A module is a directory. For each module you are given its files, its exported\n\
         symbols, the other in-repo modules it imports from, and the external packages\n\
         it uses. You do NOT see source code — infer domains from names + structure alone.\n\n\
         Identify the distinct business/functional domains the code implements (e.g.\n\
         Authentication, Billing, Notifications, Search, Reporting) and assign the modules\n\
         that implement each. Use the EXACT module keys given below. A module belongs to\n\
         at most one domain; OMIT generic infrastructure/utility modules that aren't\n\
         domain-specific (do not invent a catch-all domain for them).\n\n\
         Return ONLY a JSON object, no prose, no code fences:\n\
         {\"domains\":[{\"name\":\"<domain>\",\"summary\":\"<=15 words\",\"modules\":[\"<exact module key>\"],\"flows\":[\"<short flow name>\"]}]}\n\n\
         Modules:\n",
    );
    s.push_str(&module_graph::render_modules(modules));
    s
}

// ===========================================================================
// Response parsing (pure, tolerant)
// ===========================================================================

/// One discovered business domain and the modules/flows the model assigned it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DomainAssignment {
    name: String,
    summary: String,
    /// Module keys as the model returned them — validated against real keys at
    /// assembly time (hallucinated paths are dropped there, not here).
    modules: Vec<String>,
    flows: Vec<String>,
}

/// Parse `{"domains":[{name,summary,modules,flows}]}` (or a bare array) out of the
/// model's output, tolerant of surrounding prose / ```json fences. Entries without
/// a non-empty `name` are dropped. Returns an empty vec on any failure (the honest
/// "I couldn't read it" signal — the caller maps that to coverage 0).
fn parse_domains(output: &str) -> Vec<DomainAssignment> {
    let json_str = match module_graph::extract_json(output) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let value: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let arr = match &value {
        Value::Object(map) => map.get("domains").and_then(|v| v.as_array()).cloned(),
        Value::Array(a) => Some(a.clone()),
        _ => None,
    };
    let arr = match arr {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for entry in arr {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string());
        let name = match name {
            Some(n) if !n.is_empty() => n,
            _ => continue,
        };
        let summary = entry
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        out.push(DomainAssignment {
            name,
            summary,
            modules: string_array(entry.get("modules")),
            flows: string_array(entry.get("flows")),
        });
    }
    out
}

/// Collect a JSON array of strings into a `Vec<String>`, trimming and dropping
/// empties. Non-arrays / absent → empty.
fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// ===========================================================================
// Envelope assembly
// ===========================================================================

/// Build the result body + coverage/posterior. Each domain's `modules` is filtered
/// to keys that actually exist in the graph (hallucinated paths dropped);
/// `coverage` = distinct real modules covered / `total_modules`; `posterior` is the
/// kernel hint, or 0 when nothing real was covered.
fn assemble(
    parsed: &[DomainAssignment],
    total_modules: usize,
    module_keys: &BTreeSet<String>,
    cached: bool,
) -> (Value, f64, f64) {
    let mut covered: BTreeSet<&str> = BTreeSet::new();
    let domains_json: Vec<Value> = parsed
        .iter()
        .map(|d| {
            let real_modules: Vec<&str> = d
                .modules
                .iter()
                .filter(|m| module_keys.contains(m.as_str()))
                .map(|m| m.as_str())
                .collect();
            for m in &real_modules {
                covered.insert(m);
            }
            json!({
                "name": d.name,
                "summary": d.summary,
                "modules": real_modules,
                "flows": d.flows,
            })
        })
        .collect();

    let covered_count = covered.len();
    let coverage = if total_modules == 0 {
        0.0
    } else {
        (covered_count as f64 / total_modules as f64).min(1.0)
    };
    let posterior = if covered_count == 0 {
        0.0
    } else {
        module_graph::KERNEL_POSTERIOR
    };
    let result = json!({
        "domains": domains_json,
        "total_modules": total_modules,
        "covered_modules": covered_count,
        "domain_count": parsed.len(),
        "cached": cached,
    });
    (result, posterior, coverage)
}

fn make_envelope(
    query: &str,
    scope_key: &str,
    parsed: &[DomainAssignment],
    total_modules: usize,
    module_keys: &BTreeSet<String>,
    cached: bool,
) -> Envelope {
    let (mut result, posterior, coverage) = assemble(parsed, total_modules, module_keys, cached);
    if let Value::Object(map) = &mut result {
        map.insert("scope".to_string(), json!(scope_key));
    }
    Envelope::kernel(query, OBSERVER, PROVENANCE, result, posterior, coverage)
}

/// An honest cold/degraded envelope: kernel, coverage 0, posterior 0, never an
/// assertion that the repo "has no domains".
fn cold_envelope(query: &str, reason: &str) -> Envelope {
    Envelope::kernel(
        query,
        OBSERVER,
        PROVENANCE,
        json!({
            "domains": [],
            "total_modules": 0,
            "covered_modules": 0,
            "domain_count": 0,
            "reason": reason,
        }),
        0.0,
        0.0,
    )
}

// ===========================================================================
// Process-local fingerprint cache
// ===========================================================================

static CACHE: Lazy<Mutex<BTreeMap<u64, Vec<DomainAssignment>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

fn cache_get(fp: u64) -> Option<Vec<DomainAssignment>> {
    CACHE.lock().ok().and_then(|m| m.get(&fp).cloned())
}

fn cache_put(fp: u64, domains: Vec<DomainAssignment>) {
    // Don't cache an empty (failed) mapping — let the next call retry.
    if domains.is_empty() {
        return;
    }
    if let Ok(mut m) = CACHE.lock() {
        m.insert(fp, domains);
    }
}

// ===========================================================================
// Tests (pure — no LLM call)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prompt_contains_modules_and_domain_json_instruction() {
        let mods = vec![ModuleSummary {
            module: "src/auth".into(),
            files: vec!["login.ts".into()],
            exports: vec!["authenticate".into()],
            internal_deps: vec![],
            external_pkgs: vec!["jsonwebtoken".into()],
            export_count: 1,
        }];
        let p = build_prompt(&mods);
        assert!(p.contains("src/auth"));
        assert!(p.contains("authenticate"));
        assert!(p.contains("BUSINESS DOMAINS"));
        assert!(p.contains("\"domains\""));
    }

    #[test]
    fn parse_clean_domains_object() {
        let out = r#"{"domains":[
            {"name":"Authentication","summary":"login + tokens","modules":["src/auth","src/services/login"],"flows":["user login","token refresh"]},
            {"name":"Billing","summary":"invoices","modules":["src/billing"],"flows":["charge"]}
        ]}"#;
        let parsed = parse_domains(out);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "Authentication");
        assert_eq!(parsed[0].modules, vec!["src/auth", "src/services/login"]);
        assert_eq!(parsed[0].flows, vec!["user login", "token refresh"]);
        assert_eq!(parsed[1].name, "Billing");
    }

    #[test]
    fn parse_tolerates_prose_and_fences_and_drops_nameless() {
        let out = "Here you go:\n```json\n{\"domains\":[{\"name\":\"Search\",\"modules\":[\"src/search\"]},{\"summary\":\"no name\"}]}\n```";
        let parsed = parse_domains(out);
        assert_eq!(parsed.len(), 1, "the name-less entry is dropped");
        assert_eq!(parsed[0].name, "Search");
        // missing summary/flows default to empty.
        assert_eq!(parsed[0].summary, "");
        assert!(parsed[0].flows.is_empty());
    }

    #[test]
    fn parse_garbage_returns_empty() {
        assert!(parse_domains("I can't do that").is_empty());
        assert!(parse_domains("").is_empty());
        assert!(parse_domains("{nope").is_empty());
    }

    #[test]
    fn assemble_drops_hallucinated_modules_and_counts_real_coverage() {
        let parsed = vec![
            DomainAssignment {
                name: "Auth".into(),
                summary: "s".into(),
                modules: vec!["src/auth".into(), "src/ghost".into()], // ghost is hallucinated
                flows: vec!["login".into()],
            },
            DomainAssignment {
                name: "Billing".into(),
                summary: "s".into(),
                modules: vec!["src/billing".into()],
                flows: vec![],
            },
        ];
        let real = keys(&["src/auth", "src/billing", "src/util", "src/api"]);
        // 2 real covered (src/auth, src/billing) of 4 total → coverage 0.5.
        let (result, posterior, coverage) = assemble(&parsed, 4, &real, false);
        assert!((coverage - 0.5).abs() < 1e-9);
        assert!((posterior - module_graph::KERNEL_POSTERIOR).abs() < 1e-9);
        assert_eq!(result["covered_modules"], json!(2));
        assert_eq!(result["domain_count"], json!(2));
        // The hallucinated module is dropped from the Auth domain's output.
        let auth_modules = result["domains"][0]["modules"].as_array().unwrap();
        assert_eq!(auth_modules.len(), 1);
        assert_eq!(auth_modules[0], json!("src/auth"));
    }

    #[test]
    fn assemble_all_hallucinated_is_zero_coverage_and_posterior() {
        let parsed = vec![DomainAssignment {
            name: "Ghosts".into(),
            summary: "s".into(),
            modules: vec!["src/nope".into()],
            flows: vec![],
        }];
        let real = keys(&["src/auth"]);
        let (_r, posterior, coverage) = assemble(&parsed, 1, &real, false);
        assert_eq!(coverage, 0.0, "no real module covered → coverage 0");
        assert_eq!(posterior, 0.0, "nothing real covered → posterior 0");
    }

    #[test]
    fn envelope_is_advisory_kernel_low_authorial() {
        let parsed = vec![DomainAssignment {
            name: "Auth".into(),
            summary: "s".into(),
            modules: vec!["src/auth".into()],
            flows: vec![],
        }];
        let real = keys(&["src/auth", "src/billing"]);
        let env = make_envelope("domains", "scope-key", &parsed, 2, &real, false);
        assert!(env.kernel, "Ξ_Domain envelope MUST be kernel:true");
        assert!(env.posterior < 1.0, "kernel posterior must be < 1");
        assert_eq!(env.observer, OBSERVER);
        assert_eq!(env.provenance, PROVENANCE);
        assert_eq!(env.credibility.authorial, "low");
        assert_eq!(env.credibility.boundary, "low");
        assert_eq!(env.credibility.causal, "medium");
        assert_eq!(env.result["scope"], json!("scope-key"));
        // 1 real of 2 total → coverage 0.5.
        assert!((env.coverage - 0.5).abs() < 1e-9);
    }

    #[test]
    fn cold_envelope_does_not_assert_absence() {
        let env = cold_envelope("domains", "empty / cold graph (no modules)");
        assert!(env.kernel);
        assert_eq!(env.coverage, 0.0);
        assert_eq!(env.posterior, 0.0);
        assert_eq!(env.result["domain_count"], json!(0));
        assert!(env.result.get("reason").is_some());
    }

    #[test]
    fn cache_skips_empty_and_roundtrips_nonempty() {
        let fp = 0xDADA_BEEF_1234_5678u64;
        cache_put(fp, Vec::new());
        assert!(cache_get(fp).is_none(), "empty mapping is not cached");
        let domains = vec![DomainAssignment {
            name: "Auth".into(),
            summary: "s".into(),
            modules: vec!["src/auth".into()],
            flows: vec![],
        }];
        cache_put(fp, domains.clone());
        assert_eq!(cache_get(fp), Some(domains));
    }
}
