//! `Ξ_Layering` — architecture-drift advisory observer (Phase 4a / Pillar 3 of the
//! twin-ast plan).
//!
//! Where `Ξ_Arch` *labels* each module with an architectural layer, `Ξ_Layering`
//! takes those labels and evaluates the **layering-allowed-edge relation Φ** over
//! the repo's resolved cross-module dependency digraph: it flags **layer breaches**
//! (an edge that violates the allowed direction, e.g. `ui → data` skipping the
//! service layer) and **layer cycles** (a strongly-connected component spanning ≥2
//! distinct layers). The module labels come from the shared
//! [`super::arch_classify::classify_layers`] seam (the same forced Claude-CLI path
//! as `Ξ_Arch`); the edge set comes from the deterministic resolver
//! ([`super::module_graph::cross_module_edges`], UNCAPPED — it must NOT use the
//! prompt-budget-capped `internal_deps`).
//!
//! **Advisory, never gate-worthy** — like its siblings the envelope is always
//! `kernel:true`, `posterior<1`, `credibility=(causal:medium, authorial:low,
//! boundary:low)`. There is NO permit/deny/decision/gate code path in this file:
//! the labels are a model hint, so a "breach" is a drift *signal*, never a verdict.
//! coord receives the kernel envelope and a top-level `drift_class` wire token
//! (`none` / `in_place` / `divergent` / `unknown`) and decides what (if anything)
//! to do with it.
//!
//! **Coverage is honest about unknowns.** An edge whose endpoint has no label (the
//! module wasn't classified / was capped out) or a label of `"unknown"` is *carved
//! out* — never counted as a breach, but it lowers coverage. A `"utility"` endpoint
//! (import-by-anyone) is also carved (clean, and counts as judged). `coverage` =
//! judged edges / total edges; `posterior` is the uncalibrated kernel hint, or 0
//! when nothing was judged.
//!
//! Route: `POST /code-graph/layer-drift` — `{scope?, max_modules?}`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use axum::{extract::State, routing::post, Json, Router};
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde_json::{json, Value};

use super::module_graph::{self, ModuleSummary};
use super::{arch_classify, code_graph_api, scope_project_dir, Envelope};
use crate::mcp::types::ApiState;

/// Observer name + provenance for the kernel envelope.
const OBSERVER: &str = "layer";
const PROVENANCE: &str = super::PROV_KERNEL_LAYER;

/// The structural layers Φ reasons over. `utility` and `unknown` are deliberately
/// NOT here — they're handled by the carve-out, not the allowed-edge relation.
const STRUCTURAL_LAYERS: &[&str] = &["ui", "api", "service", "data"];

// ===========================================================================
// Request / route
// ===========================================================================

#[derive(Debug, Deserialize, Default)]
pub struct LayerDriftReq {
    /// Optional `(repo,language)` scope selector — a repo name/slug (cross-repo
    /// `Ξ_AST`), a project dir, or a tsconfig path.
    pub scope: Option<String>,
    /// Cap on modules classified for layer labels in one pass (default
    /// [`module_graph::DEFAULT_MAX_MODULES`], clamped to 1..=200). The Φ edge set is
    /// always the UNCAPPED digraph; the cap only bounds the label prompt.
    pub max_modules: Option<usize>,
}

/// Routes contributed alongside the `/code-graph/*` surface.
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/code-graph/layer-drift", post(layer_drift))
}

/// POST /code-graph/layer-drift → kernel envelope of layer-drift findings.
async fn layer_drift(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<LayerDriftReq>,
) -> Json<Envelope> {
    let q = "layer_drift";

    let scope = match code_graph_api::resolve_project_scope(req.scope.as_deref(), None) {
        Some(s) => s,
        None => return Json(cold_envelope(q, "", "no resolvable scope (cold)")),
    };
    let project_dir = scope_project_dir(&scope);
    let max_modules = req
        .max_modules
        .unwrap_or(module_graph::DEFAULT_MAX_MODULES)
        .clamp(1, 200);

    // Build the resolved graph ONCE → summaries (for labels), total, fingerprint,
    // and the UNCAPPED cross-module edge set (for Φ + cycles).
    let (summaries, total_modules, fingerprint, edges) =
        match module_graph::build_layer_inputs(project_dir).await {
            Some(t) => t,
            None => return Json(cold_envelope(q, &scope.key, "graph build failed")),
        };

    // A cold graph (no modules or no cross-module edges) → honest "unknown", never a
    // false "no breaches" assertion.
    if total_modules == 0 || edges.is_empty() {
        return Json(cold_envelope(
            q,
            &scope.key,
            "empty / cold graph (no cross-module edges)",
        ));
    }

    // Fingerprint cache: a prior identical graph already evaluated → free.
    if let Some(cached) = cache_get(fingerprint) {
        return Json(make_envelope(q, &scope.key, &edges, &cached, true));
    }

    // Cap to the highest-signal modules for the LABEL prompt only; the Φ edge set
    // above stays uncapped. Get module→layer labels via the shared classify seam.
    let capped: Vec<ModuleSummary> = summaries.into_iter().take(max_modules).collect();
    let assignments = arch_classify::classify_layers(&capped).await;
    let layer_of: HashMap<String, String> = assignments
        .iter()
        .map(|a| (a.module.clone(), a.layer.clone()))
        .collect();

    cache_put(fingerprint, layer_of.clone());
    Json(make_envelope(q, &scope.key, &edges, &layer_of, false))
}

// ===========================================================================
// Φ — the layering-allowed-edge relation (pure)
// ===========================================================================

/// The allowed-edge relation over the STRUCTURAL layers (`ui/api/service/data`).
///
/// An edge `a → b` is allowed iff it is intra-layer (`a == b`) or a sanctioned
/// downward dependency. `utility`/`unknown` endpoints are NOT decided here — the
/// caller's carve-out handles them before Φ is consulted.
fn is_allowed(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    const ALLOWED_CORE: &[(&str, &str)] = &[
        ("ui", "api"),
        ("ui", "service"),
        ("api", "service"),
        ("service", "data"),
    ];
    ALLOWED_CORE.contains(&(a, b))
}

/// Is this a structural layer Φ reasons over (`ui/api/service/data`)?
fn is_structural(layer: &str) -> bool {
    STRUCTURAL_LAYERS.contains(&layer)
}

/// Classification of a single edge under Φ + the carve-out.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EdgeVerdict {
    /// Both endpoints structural + allowed (intra-layer or sanctioned downward).
    Clean,
    /// An endpoint is `utility` (import-by-anyone) — judged, clean, never a breach.
    CarvedUtility,
    /// An endpoint is unlabelled or `"unknown"` — NOT judged (lowers coverage).
    CarvedUnknown,
    /// Both endpoints structural, edge violates Φ — a layer breach.
    Breach {
        from_layer: String,
        to_layer: String,
    },
}

/// Evaluate one edge `(from, to)` under Φ + the carve-out, given the labels.
fn classify_edge(from: &str, to: &str, layer_of: &HashMap<String, String>) -> EdgeVerdict {
    let la = layer_of.get(from).map(|s| s.as_str());
    let lb = layer_of.get(to).map(|s| s.as_str());

    // Unlabelled or explicit "unknown" → carved as unknown (lowers coverage,
    // never a breach).
    let la = match la {
        Some(l) if l != "unknown" => l,
        _ => return EdgeVerdict::CarvedUnknown,
    };
    let lb = match lb {
        Some(l) if l != "unknown" => l,
        _ => return EdgeVerdict::CarvedUnknown,
    };

    // Utility (import-by-anyone) → carved clean, counts as judged.
    if la == "utility" || lb == "utility" {
        return EdgeVerdict::CarvedUtility;
    }

    // Both structural → apply Φ.
    if is_allowed(la, lb) {
        EdgeVerdict::Clean
    } else {
        EdgeVerdict::Breach {
            from_layer: la.to_string(),
            to_layer: lb.to_string(),
        }
    }
}

/// A layer breach: a single dependency edge that violates Φ.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Breach {
    from: String,
    to: String,
    from_layer: String,
    to_layer: String,
}

/// The outcome of running Φ over every edge: the breaches, the count of judged
/// edges (both endpoints structural-or-utility, i.e. not carved-unknown), and the
/// count of utility/unknown carve-outs.
struct PhiResult {
    breaches: Vec<Breach>,
    judged_edges: usize,
    carved_utility_or_unknown: usize,
}

/// Run Φ over the full (uncapped) edge set. Pure — feed it a fixture edge set + a
/// stub `layer_of` and it is fully testable without the model.
fn evaluate_phi(
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
) -> PhiResult {
    let mut breaches = Vec::new();
    let mut judged = 0usize;
    let mut carved = 0usize;
    for (from, to) in edges {
        match classify_edge(from, to, layer_of) {
            EdgeVerdict::Clean => judged += 1,
            EdgeVerdict::CarvedUtility => {
                judged += 1;
                carved += 1;
            }
            EdgeVerdict::CarvedUnknown => carved += 1,
            EdgeVerdict::Breach {
                from_layer,
                to_layer,
            } => {
                judged += 1;
                breaches.push(Breach {
                    from: from.clone(),
                    to: to.clone(),
                    from_layer,
                    to_layer,
                });
            }
        }
    }
    breaches.sort_by(|x, y| x.from.cmp(&y.from).then_with(|| x.to.cmp(&y.to)));
    PhiResult {
        breaches,
        judged_edges: judged,
        carved_utility_or_unknown: carved,
    }
}

// ===========================================================================
// Cycle detection — Tarjan SCC over the full digraph (pure)
// ===========================================================================

/// A cross-layer dependency cycle: a strongly-connected component (>1 member) that
/// spans ≥2 distinct STRUCTURAL layers (per the labels, ignoring utility/unknown
/// members). A same-layer or utility/unknown-only SCC is informational, not a
/// breach, so it is excluded here.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LayerCycle {
    members: Vec<String>,
}

/// Tarjan SCC state over an integer-indexed adjacency list. Bundling the working
/// state in a struct keeps the recursive `strongconnect` a 1-arg method (no
/// `clippy::too_many_arguments`) and the borrows local.
struct Tarjan<'a> {
    adj: &'a [Vec<usize>],
    index: Vec<Option<usize>>,
    lowlink: Vec<usize>,
    on_stack: Vec<bool>,
    stack: Vec<usize>,
    next_index: usize,
    sccs: Vec<Vec<usize>>,
}

impl<'a> Tarjan<'a> {
    fn new(adj: &'a [Vec<usize>]) -> Self {
        let n = adj.len();
        Tarjan {
            adj,
            index: vec![None; n],
            lowlink: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            next_index: 0,
            sccs: Vec::new(),
        }
    }

    /// Compute all SCCs and consume the state.
    fn run(mut self) -> Vec<Vec<usize>> {
        for v in 0..self.adj.len() {
            if self.index[v].is_none() {
                self.strongconnect(v);
            }
        }
        self.sccs
    }

    fn strongconnect(&mut self, v: usize) {
        self.index[v] = Some(self.next_index);
        self.lowlink[v] = self.next_index;
        self.next_index += 1;
        self.stack.push(v);
        self.on_stack[v] = true;

        for i in 0..self.adj[v].len() {
            let w = self.adj[v][i];
            match self.index[w] {
                None => {
                    self.strongconnect(w);
                    self.lowlink[v] = self.lowlink[v].min(self.lowlink[w]);
                }
                Some(w_idx) => {
                    if self.on_stack[w] {
                        self.lowlink[v] = self.lowlink[v].min(w_idx);
                    }
                }
            }
        }

        if self.index[v] == Some(self.lowlink[v]) {
            let mut component = Vec::new();
            loop {
                let w = self.stack.pop().unwrap();
                self.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            self.sccs.push(component);
        }
    }
}

/// Find cross-layer cycles via Tarjan's SCC over the full edge digraph. Recursion is
/// bounded by the module count (well within stack limits for any real repo). Pure —
/// testable from a fixture edge set + stub labels.
fn find_layer_cycles(
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
) -> Vec<LayerCycle> {
    // Stable, deterministic node indexing (BTreeSet → sorted node order).
    let mut node_set: BTreeSet<&str> = BTreeSet::new();
    for (from, to) in edges {
        node_set.insert(from.as_str());
        node_set.insert(to.as_str());
    }
    let nodes: Vec<&str> = node_set.into_iter().collect();
    let index_of: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();

    // Integer-indexed adjacency list.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (from, to) in edges {
        adj[index_of[from.as_str()]].push(index_of[to.as_str()]);
    }

    let sccs = Tarjan::new(&adj).run();

    // Keep only multi-member SCCs that span ≥2 distinct structural layers.
    let mut cycles = Vec::new();
    for comp in sccs {
        if comp.len() < 2 {
            continue;
        }
        let mut members: Vec<String> = comp.iter().map(|&i| nodes[i].to_string()).collect();
        let distinct_structural: BTreeSet<&str> = members
            .iter()
            .filter_map(|m| layer_of.get(m).map(|s| s.as_str()))
            .filter(|&l| is_structural(l))
            .collect();
        if distinct_structural.len() >= 2 {
            members.sort();
            cycles.push(LayerCycle { members });
        }
    }
    cycles.sort_by(|a, b| a.members.cmp(&b.members));
    cycles
}

// ===========================================================================
// Envelope assembly (pure)
// ===========================================================================

/// The coord wire token for the top-level `drift_class`, by precedence: any cycle
/// → `divergent`; else any breach → `in_place`; else any unjudged edge → `unknown`;
/// else → `none`.
fn drift_class(
    breaches: usize,
    cycles: usize,
    judged_edges: usize,
    total_edges: usize,
) -> &'static str {
    if cycles > 0 {
        "divergent"
    } else if breaches > 0 {
        "in_place"
    } else if judged_edges < total_edges {
        "unknown"
    } else {
        "none"
    }
}

/// Build the result body + coverage/posterior from the (uncapped) edge set + labels.
/// Pure — the tests drive this directly with fixture edges + a stub `layer_of`.
fn assemble(
    scope_key: &str,
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
    cached: bool,
) -> (Value, f64, f64) {
    let total_edges = edges.len();
    let phi = evaluate_phi(edges, layer_of);
    let cycles = find_layer_cycles(edges, layer_of);
    let judged_edges = phi.judged_edges;

    let coverage = if total_edges == 0 {
        0.0
    } else {
        (judged_edges as f64 / total_edges as f64).min(1.0)
    };
    let posterior = if judged_edges == 0 {
        0.0
    } else {
        module_graph::KERNEL_POSTERIOR
    };

    let breaches_json: Vec<Value> = phi
        .breaches
        .iter()
        .map(|b| {
            json!({
                "from": b.from,
                "to": b.to,
                "from_layer": b.from_layer,
                "to_layer": b.to_layer,
                "class": "arch:layer_breach",
            })
        })
        .collect();
    let cycles_json: Vec<Value> = cycles
        .iter()
        .map(|c| json!({ "members": c.members, "class": "arch:layer_cycle" }))
        .collect();

    // The layer map only carries known (label-present) modules touched by the graph.
    let layer_map: BTreeMap<&str, &str> = layer_of
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let cls = drift_class(phi.breaches.len(), cycles.len(), judged_edges, total_edges);

    let result = json!({
        "scope": scope_key,
        "drift_class": cls,
        "breaches": breaches_json,
        "cycles": cycles_json,
        "layer_map": layer_map,
        "carve_out": {
            "utility_or_unknown_edges": phi.carved_utility_or_unknown,
            "note": "edges with a utility endpoint (import-by-anyone) or an unlabelled/unknown endpoint are carved out — never a breach; unknown-endpoint edges lower coverage",
        },
        "total_edges": total_edges,
        "judged_edges": judged_edges,
        "cached": cached,
    });
    (result, posterior, coverage)
}

fn make_envelope(
    query: &str,
    scope_key: &str,
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
    cached: bool,
) -> Envelope {
    let (result, posterior, coverage) = assemble(scope_key, edges, layer_of, cached);
    Envelope::kernel(query, OBSERVER, PROVENANCE, result, posterior, coverage)
}

/// An honest cold/degraded envelope: kernel, coverage 0, posterior 0, never a false
/// "no breaches" assertion. A cold graph is `drift_class:"unknown"`, NOT `"none"`.
fn cold_envelope(query: &str, scope_key: &str, reason: &str) -> Envelope {
    Envelope::kernel(
        query,
        OBSERVER,
        PROVENANCE,
        json!({
            "scope": scope_key,
            "drift_class": "unknown",
            "breaches": [],
            "cycles": [],
            "layer_map": {},
            "carve_out": { "utility_or_unknown_edges": 0, "note": "cold graph — no edges judged" },
            "total_edges": 0,
            "judged_edges": 0,
            "reason": reason,
        }),
        0.0,
        0.0,
    )
}

// ===========================================================================
// Process-local fingerprint cache
// ===========================================================================

static CACHE: Lazy<Mutex<BTreeMap<u64, HashMap<String, String>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

fn cache_get(fp: u64) -> Option<HashMap<String, String>> {
    CACHE.lock().ok().and_then(|m| m.get(&fp).cloned())
}

fn cache_put(fp: u64, layer_of: HashMap<String, String>) {
    // Don't cache an empty (failed) classification — let the next call retry.
    if layer_of.is_empty() {
        return;
    }
    if let Ok(mut m) = CACHE.lock() {
        m.insert(fp, layer_of);
    }
}

// ===========================================================================
// Tests (pure — no LLM call; fixture edges + stub layer_of maps)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(m, l)| (m.to_string(), l.to_string()))
            .collect()
    }

    #[test]
    fn is_allowed_relation() {
        // intra-layer always allowed.
        assert!(is_allowed("service", "service"));
        // sanctioned downward.
        assert!(is_allowed("ui", "api"));
        assert!(is_allowed("ui", "service"));
        assert!(is_allowed("api", "service"));
        assert!(is_allowed("service", "data"));
        // skipping / upward NOT allowed.
        assert!(!is_allowed("ui", "data"));
        assert!(!is_allowed("data", "service"));
        assert!(!is_allowed("api", "ui"));
    }

    // 1. ui→data (skip) = breach / in_place; ui→api→service→data chain = clean / none.
    #[test]
    fn skip_layer_is_breach_and_downward_chain_is_clean() {
        let layer_of = labels(&[
            ("m_ui", "ui"),
            ("m_api", "api"),
            ("m_svc", "service"),
            ("m_data", "data"),
        ]);

        let breach_edges = edges(&[("m_ui", "m_data")]);
        let (result, _p, _c) = assemble("s", &breach_edges, &layer_of, false);
        assert_eq!(result["drift_class"], json!("in_place"));
        assert_eq!(result["breaches"].as_array().unwrap().len(), 1);
        assert_eq!(result["breaches"][0]["class"], json!("arch:layer_breach"));
        assert_eq!(result["breaches"][0]["from_layer"], json!("ui"));
        assert_eq!(result["breaches"][0]["to_layer"], json!("data"));

        let chain = edges(&[("m_ui", "m_api"), ("m_api", "m_svc"), ("m_svc", "m_data")]);
        let (result, _p, coverage) = assemble("s", &chain, &layer_of, false);
        assert_eq!(result["drift_class"], json!("none"));
        assert!(result["breaches"].as_array().unwrap().is_empty());
        assert_eq!(result["judged_edges"], json!(3));
        assert!(
            (coverage - 1.0).abs() < 1e-9,
            "all edges judged → coverage 1"
        );
    }

    // 2. data→service (upward) = breach; service→data (downward) = clean.
    #[test]
    fn upward_is_breach_downward_is_clean() {
        let layer_of = labels(&[("m_svc", "service"), ("m_data", "data")]);

        let up = edges(&[("m_data", "m_svc")]);
        let (result, _p, _c) = assemble("s", &up, &layer_of, false);
        assert_eq!(result["drift_class"], json!("in_place"));
        assert_eq!(result["breaches"].as_array().unwrap().len(), 1);

        let down = edges(&[("m_svc", "m_data")]);
        let (result, _p, _c) = assemble("s", &down, &layer_of, false);
        assert_eq!(result["drift_class"], json!("none"));
        assert!(result["breaches"].as_array().unwrap().is_empty());
    }

    // 3. any → utility, and utility → anything = carved, never a breach.
    #[test]
    fn utility_endpoints_are_carved_never_a_breach() {
        let layer_of = labels(&[("m_ui", "ui"), ("m_util", "utility"), ("m_data", "data")]);
        // ui→utility and utility→data: both carved (utility = import-by-anyone),
        // judged + clean → no breach, drift_class none, coverage 1.
        let e = edges(&[("m_ui", "m_util"), ("m_util", "m_data")]);
        let (result, posterior, coverage) = assemble("s", &e, &layer_of, false);
        assert!(result["breaches"].as_array().unwrap().is_empty());
        assert_eq!(result["drift_class"], json!("none"));
        assert_eq!(result["judged_edges"], json!(2));
        assert_eq!(result["carve_out"]["utility_or_unknown_edges"], json!(2));
        assert!((coverage - 1.0).abs() < 1e-9);
        assert!((posterior - module_graph::KERNEL_POSTERIOR).abs() < 1e-9);
    }

    // 4. edge touching an unknown/unlabelled module = arch:unknown_edge (carved),
    //    lowers coverage, drift_class unknown, never a false breach.
    #[test]
    fn unknown_endpoint_lowers_coverage_and_is_not_a_breach() {
        // m_x has no label; m_data is data. m_svc→m_data is judged + clean.
        let layer_of = labels(&[("m_svc", "service"), ("m_data", "data")]);
        let e = edges(&[("m_x", "m_data"), ("m_svc", "m_data")]);
        let (result, _p, coverage) = assemble("s", &e, &layer_of, false);
        assert!(
            result["breaches"].as_array().unwrap().is_empty(),
            "an unknown-endpoint edge is never a breach"
        );
        assert_eq!(result["total_edges"], json!(2));
        assert_eq!(result["judged_edges"], json!(1));
        assert_eq!(result["drift_class"], json!("unknown"));
        assert!(coverage < 1.0, "unknown-endpoint edge lowers coverage");
        assert!((coverage - 0.5).abs() < 1e-9);

        // explicit "unknown" label is treated the same as unlabelled.
        let layer_of2 = labels(&[("m_x", "unknown"), ("m_data", "data")]);
        let e2 = edges(&[("m_x", "m_data")]);
        let (result2, _p2, c2) = assemble("s", &e2, &layer_of2, false);
        assert!(result2["breaches"].as_array().unwrap().is_empty());
        assert_eq!(result2["judged_edges"], json!(0));
        assert_eq!(c2, 0.0);
    }

    // 5. cross-layer cycle (api↔data) = layer_cycle / divergent with full SCC;
    //    same-layer 2-cycle = NOT a breach.
    #[test]
    fn cross_layer_cycle_is_divergent_same_layer_cycle_is_not() {
        let layer_of = labels(&[("m_api", "api"), ("m_data", "data")]);
        let e = edges(&[("m_api", "m_data"), ("m_data", "m_api")]);
        let (result, _p, _c) = assemble("s", &e, &layer_of, false);
        assert_eq!(result["drift_class"], json!("divergent"));
        let cycles = result["cycles"].as_array().unwrap();
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0]["class"], json!("arch:layer_cycle"));
        let members = cycles[0]["members"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], json!("m_api"));
        assert_eq!(members[1], json!("m_data"));

        // same-layer 2-cycle: both service → SCC spans only 1 structural layer → NOT
        // a cycle finding (informational).
        let layer_same = labels(&[("m_a", "service"), ("m_b", "service")]);
        let e2 = edges(&[("m_a", "m_b"), ("m_b", "m_a")]);
        let (result2, _p2, _c2) = assemble("s", &e2, &layer_same, false);
        assert!(
            result2["cycles"].as_array().unwrap().is_empty(),
            "a same-layer cycle is informational, not a breach"
        );
        // ...but note service→service edges are clean, so drift_class is none.
        assert_eq!(result2["drift_class"], json!("none"));
    }

    // 6. D2 regression: a module with 9+ resolved cross-layer edges — the 9th breach
    //    is still flagged (Φ uses the uncapped edge set, not capped internal_deps).
    #[test]
    fn ninth_cross_layer_breach_is_still_flagged() {
        // m_ui (ui) → 9 distinct data modules. ui→data is a breach (skips api/service).
        let mut layer_pairs = vec![("m_ui", "ui")];
        let mut edge_pairs = Vec::new();
        let data_mods: Vec<String> = (0..9).map(|i| format!("m_data{i}")).collect();
        for d in &data_mods {
            layer_pairs.push((d.as_str(), "data"));
            edge_pairs.push(("m_ui", d.as_str()));
        }
        let layer_of = labels(&layer_pairs);
        let e = edges(&edge_pairs);
        let (result, _p, _c) = assemble("s", &e, &layer_of, false);
        let breaches = result["breaches"].as_array().unwrap();
        assert_eq!(
            breaches.len(),
            9,
            "all 9 cross-layer breaches flagged — proves uncapped edge set"
        );
        assert_eq!(result["drift_class"], json!("in_place"));
        // The 9th breach edge (to m_data8) is present.
        assert!(breaches
            .iter()
            .any(|b| b["to"] == json!("m_data8") && b["class"] == json!("arch:layer_breach")));
    }

    // 7. envelope discipline: kernel:true, posterior<1, credibility (medium,low,low);
    //    cold graph → coverage 0, no false "no breaches".
    #[test]
    fn envelope_is_advisory_kernel_and_cold_is_honest() {
        let layer_of = labels(&[("m_ui", "ui"), ("m_api", "api")]);
        let e = edges(&[("m_ui", "m_api")]);
        let env = make_envelope("layer_drift", "scope-key", &e, &layer_of, false);
        assert!(env.kernel, "Ξ_Layering envelope MUST be kernel:true");
        assert!(env.posterior < 1.0, "kernel posterior must be < 1");
        assert_eq!(env.observer, OBSERVER);
        assert_eq!(env.provenance, PROVENANCE);
        assert_eq!(env.credibility.causal, "medium");
        assert_eq!(env.credibility.authorial, "low");
        assert_eq!(env.credibility.boundary, "low");
        assert_eq!(env.result["scope"], json!("scope-key"));

        // Cold graph → coverage 0, drift_class unknown, breaches empty but with a
        // reason — never a false "no breaches" assertion.
        let cold = cold_envelope(
            "layer_drift",
            "scope-key",
            "empty / cold graph (no cross-module edges)",
        );
        assert!(cold.kernel);
        assert_eq!(cold.coverage, 0.0);
        assert_eq!(cold.posterior, 0.0);
        assert_eq!(cold.result["drift_class"], json!("unknown"));
        assert!(cold.result["breaches"].as_array().unwrap().is_empty());
        assert!(cold.result.get("reason").is_some());
    }

    #[test]
    fn cache_skips_empty_and_roundtrips_nonempty() {
        let fp = 0x1A2B_3C4D_5E6F_7788u64;
        cache_put(fp, HashMap::new());
        assert!(
            cache_get(fp).is_none(),
            "empty classification is not cached"
        );
        let layer_of = labels(&[("m_a", "api")]);
        cache_put(fp, layer_of.clone());
        assert_eq!(cache_get(fp), Some(layer_of));
    }
}
