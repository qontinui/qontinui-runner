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
//! **Two paths, gated by the declared side (the credibility rule made concrete):**
//! - **4a — no manifest (advisory):** labels come from the `Ξ_Arch` kernel (a model
//!   hint), so the envelope is `kernel:true`, `credibility=(medium,low,low)`, and
//!   carries NO gate recommendation — a "breach" is a drift *signal*, never a verdict.
//! - **4b — `<repo>/.qontinui/layers.toml` present (gate-grade):** labels come from
//!   the AUTHORED manifest (the spec), so breach detection is deterministic over the
//!   resolved graph. The envelope is `kernel:false` with the resolver's credibility
//!   `(high,high,medium)` and a `gate` recommendation (`block`/`escalate`/`allow`).
//!   The manifest's `exempt` modules are excluded from Φ AND cycles; `[carve_out]`
//!   edges and the `[order]` override are honored.
//!
//! Both paths emit the coord wire token `drift_class` (`none`/`in_place`/`divergent`/
//! `unknown`); coord decides what to do with it (advisory hint vs. gated block).
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

use super::layer_manifest::LayerManifest;
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

    // Phase 4b: load the AUTHORED declared side, if the repo ships one. Read it
    // before `build_layer_inputs` consumes `project_dir`.
    let manifest = LayerManifest::load(&project_dir);

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

    // Phase 4b — AUTHORED gate-grade path: a manifest is the declared side (the
    // spec), so labelling is deterministic (no model call, no cache), the verdict is
    // gate-grade (kernel:false), and it carries a block/escalate/allow recommendation.
    if let Some(manifest) = manifest {
        let layer_of = label_from_manifest(&edges, &manifest);
        return Json(make_gate_envelope(
            q, &scope.key, &edges, &layer_of, &manifest,
        ));
    }

    // Phase 4a — KERNEL advisory path (no manifest): Ξ_Arch labels are a model hint,
    // so the verdict stays advisory (kernel:true) and NEVER recommends a gate.
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
    /// (4b) An endpoint is an `exempt` module (composition root/barrel) — judged,
    /// clean, never a breach; also excluded from cycle detection.
    CarvedExempt,
    /// (4b) The edge matches a manifest `[carve_out]` rule — an accepted exception,
    /// judged, never a breach.
    CarvedException,
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
            EdgeVerdict::CarvedUtility
            | EdgeVerdict::CarvedExempt
            | EdgeVerdict::CarvedException => {
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
// Phase 4b — authored gate-grade path (manifest declared side)
// ===========================================================================

/// Label every module node in `edges` from the manifest's `[layers]` globs. Modules
/// with no matching glob are omitted (treated as unlabelled → a manifest coverage gap
/// that lowers coverage honestly).
fn label_from_manifest(
    edges: &BTreeSet<(String, String)>,
    manifest: &LayerManifest,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (a, b) in edges {
        for m in [a, b] {
            if !out.contains_key(m) {
                if let Some(layer) = manifest.layer_of(m) {
                    out.insert(m.clone(), layer.to_string());
                }
            }
        }
    }
    out
}

/// Φ for one edge under the AUTHORED manifest: exempt endpoints and `[carve_out]`
/// matches are accepted (never breaches); otherwise the structural Φ with the
/// manifest's allowed-edge relation.
fn classify_edge_manifest(
    from: &str,
    to: &str,
    layer_of: &HashMap<String, String>,
    manifest: &LayerManifest,
) -> EdgeVerdict {
    if manifest.is_exempt(from) || manifest.is_exempt(to) {
        return EdgeVerdict::CarvedExempt;
    }
    let la = match layer_of.get(from).map(|s| s.as_str()) {
        Some(l) if l != "unknown" => l,
        _ => return EdgeVerdict::CarvedUnknown,
    };
    let lb = match layer_of.get(to).map(|s| s.as_str()) {
        Some(l) if l != "unknown" => l,
        _ => return EdgeVerdict::CarvedUnknown,
    };
    if la == "utility" || lb == "utility" {
        return EdgeVerdict::CarvedUtility;
    }
    if manifest.is_carved(from, lb) {
        return EdgeVerdict::CarvedException;
    }
    if manifest.is_allowed(la, lb) {
        EdgeVerdict::Clean
    } else {
        EdgeVerdict::Breach {
            from_layer: la.to_string(),
            to_layer: lb.to_string(),
        }
    }
}

/// The manifest-path Φ result, with the carve-out breakdown for the verdict body.
struct GatePhi {
    breaches: Vec<Breach>,
    judged: usize,
    exempt: usize,
    exception: usize,
    utility: usize,
    unknown: usize,
}

fn evaluate_phi_manifest(
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
    manifest: &LayerManifest,
) -> GatePhi {
    let mut g = GatePhi {
        breaches: Vec::new(),
        judged: 0,
        exempt: 0,
        exception: 0,
        utility: 0,
        unknown: 0,
    };
    for (from, to) in edges {
        match classify_edge_manifest(from, to, layer_of, manifest) {
            EdgeVerdict::Clean => g.judged += 1,
            EdgeVerdict::CarvedUtility => {
                g.judged += 1;
                g.utility += 1;
            }
            EdgeVerdict::CarvedExempt => {
                g.judged += 1;
                g.exempt += 1;
            }
            EdgeVerdict::CarvedException => {
                g.judged += 1;
                g.exception += 1;
            }
            EdgeVerdict::CarvedUnknown => g.unknown += 1,
            EdgeVerdict::Breach {
                from_layer,
                to_layer,
            } => {
                g.judged += 1;
                g.breaches.push(Breach {
                    from: from.clone(),
                    to: to.clone(),
                    from_layer,
                    to_layer,
                });
            }
        }
    }
    g.breaches
        .sort_by(|x, y| x.from.cmp(&y.from).then_with(|| x.to.cmp(&y.to)));
    g
}

/// The gate recommendation for an authored verdict: `block` on any breach/cycle (a
/// real layering violation against the authored spec); `escalate` when the manifest
/// has coverage gaps (unlabelled edges → can't fully verify); else `allow`.
fn gate_recommendation(breaches: usize, cycles: usize, unknown: usize) -> &'static str {
    if breaches > 0 || cycles > 0 {
        "block"
    } else if unknown > 0 {
        "escalate"
    } else {
        "allow"
    }
}

/// Build the GATE-GRADE envelope (`kernel:false`, resolver credibility) for the
/// authored-manifest path. Cycles run over the exempt-filtered digraph so the
/// composition root no longer anchors a mega-SCC.
fn make_gate_envelope(
    query: &str,
    scope_key: &str,
    edges: &BTreeSet<(String, String)>,
    layer_of: &HashMap<String, String>,
    manifest: &LayerManifest,
) -> Envelope {
    let total_edges = edges.len();
    let phi = evaluate_phi_manifest(edges, layer_of, manifest);

    let cycle_edges: BTreeSet<(String, String)> = edges
        .iter()
        .filter(|(a, b)| !manifest.is_exempt(a) && !manifest.is_exempt(b))
        .cloned()
        .collect();
    let cycles = find_layer_cycles(&cycle_edges, layer_of);

    let coverage = if total_edges == 0 {
        0.0
    } else {
        (phi.judged as f64 / total_edges as f64).min(1.0)
    };
    let posterior = coverage; // deterministic over the resolved graph — not a kernel hint
    let gate = gate_recommendation(phi.breaches.len(), cycles.len(), phi.unknown);
    let cls = drift_class(phi.breaches.len(), cycles.len(), phi.judged, total_edges);

    let breaches_json: Vec<Value> = phi
        .breaches
        .iter()
        .map(|b| {
            json!({ "from": b.from, "to": b.to, "from_layer": b.from_layer,
                    "to_layer": b.to_layer, "class": "arch:layer_breach" })
        })
        .collect();
    let cycles_json: Vec<Value> = cycles
        .iter()
        .map(|c| json!({ "members": c.members, "class": "arch:layer_cycle" }))
        .collect();
    let layer_map: BTreeMap<&str, &str> = layer_of
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // Manifest coverage gaps: in-scope module nodes the globs didn't label.
    let mut unlabelled: BTreeSet<&str> = BTreeSet::new();
    for (a, b) in edges {
        for m in [a.as_str(), b.as_str()] {
            if !manifest.is_exempt(m) && manifest.layer_of(m).is_none() {
                unlabelled.insert(m);
            }
        }
    }

    let result = json!({
        "scope": scope_key,
        "declared_source": "layers_toml",
        "gate": gate,
        "drift_class": cls,
        "breaches": breaches_json,
        "cycles": cycles_json,
        "layer_map": layer_map,
        "carve_out": {
            "exempt_edges": phi.exempt,
            "exception_edges": phi.exception,
            "utility_edges": phi.utility,
            "unknown_edges": phi.unknown,
            "note": "exempt = composition root/barrel (excluded from Φ + cycles); exception = manifest [carve_out]; utility = import-by-anyone; unknown = manifest coverage gap (lowers coverage)",
        },
        "unlabelled_modules": unlabelled.iter().collect::<Vec<_>>(),
        "total_edges": total_edges,
        "judged_edges": phi.judged,
    });
    Envelope::authored(
        query,
        OBSERVER,
        super::PROV_LAYER_GATE,
        result,
        posterior,
        coverage,
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

    // -----------------------------------------------------------------------
    // Phase 4b — authored gate-grade path (manifest)
    // -----------------------------------------------------------------------

    const MANIFEST: &str = r#"
[order]
allowed = [["ui","api"],["ui","service"],["api","service"],["service","data"],["api","data"]]
[layers]
"src/commands/**" = "api"
"src/database/**" = "data"
"src/util/**" = "utility"
"src/**" = "service"
[exempt]
modules = ["src"]
[carve_out]
edges = [ { from_glob = "src/database/**", to_layer = "service", reason = "persist domain types" } ]
"#;

    fn manifest() -> LayerManifest {
        LayerManifest::parse(MANIFEST).expect("manifest parses")
    }

    #[test]
    fn manifest_exempt_root_edges_are_carved_and_excluded_from_cycles() {
        let m = manifest();
        let e = edges(&[("src", "src/commands"), ("src", "src/database")]); // root → api / data
        let layer_of = label_from_manifest(&e, &m);
        let phi = evaluate_phi_manifest(&e, &layer_of, &m);
        assert!(
            phi.breaches.is_empty(),
            "exempt root edges are never breaches"
        );
        assert_eq!(phi.exempt, 2);
        // the exempt root is filtered out of the cycle graph
        let cycle_edges: BTreeSet<(String, String)> = e
            .iter()
            .filter(|(a, b)| !m.is_exempt(a) && !m.is_exempt(b))
            .cloned()
            .collect();
        assert!(cycle_edges.is_empty(), "all edges touch the exempt root");
    }

    #[test]
    fn manifest_carve_out_accepts_database_to_service() {
        let m = manifest();
        // data → service is normally a breach; the carve_out accepts it from database/**.
        let e = edges(&[("src/database/pg", "src/orchestrator")]); // data → service
        let layer_of = label_from_manifest(&e, &m);
        let phi = evaluate_phi_manifest(&e, &layer_of, &m);
        assert!(
            phi.breaches.is_empty(),
            "database→service is carved by the manifest"
        );
        assert_eq!(phi.exception, 1);
    }

    #[test]
    fn manifest_order_override_allows_api_to_data_but_still_flags_upward() {
        let m = manifest();
        let e = edges(&[
            ("src/commands", "src/database"), // api → data: sanctioned by [order] → clean
            ("src/orchestrator", "src/commands"), // service → api: still a breach
        ]);
        let layer_of = label_from_manifest(&e, &m);
        let phi = evaluate_phi_manifest(&e, &layer_of, &m);
        assert_eq!(
            phi.breaches.len(),
            1,
            "only the service→api upward edge breaches"
        );
        assert_eq!(phi.breaches[0].from, "src/orchestrator");
        assert_eq!(phi.breaches[0].from_layer, "service");
        assert_eq!(phi.breaches[0].to_layer, "api");
    }

    #[test]
    fn gate_envelope_is_authored_not_kernel_and_recommends_block_on_breach() {
        let m = manifest();
        let e = edges(&[("src/orchestrator", "src/commands")]); // service → api breach
        let layer_of = label_from_manifest(&e, &m);
        let env = make_gate_envelope("layer_drift", "scope", &e, &layer_of, &m);
        // Gate-grade: NOT a kernel; resolver credibility; deterministic posterior.
        assert!(
            !env.kernel,
            "authored verdict is gate-grade, not a kernel hint"
        );
        assert_eq!(env.credibility.authorial, "high");
        assert_eq!(env.provenance, super::super::PROV_LAYER_GATE);
        assert_eq!(env.result["declared_source"], json!("layers_toml"));
        assert_eq!(env.result["gate"], json!("block"), "a breach → block");
        assert_eq!(env.result["drift_class"], json!("in_place"));
    }

    #[test]
    fn gate_recommends_allow_when_clean_and_escalate_on_coverage_gap() {
        let m = manifest();
        // all clean + fully labelled → allow
        let clean = edges(&[("src/commands", "src/orchestrator")]); // api → service
        let env = make_gate_envelope(
            "layer_drift",
            "s",
            &clean,
            &label_from_manifest(&clean, &m),
            &m,
        );
        assert_eq!(env.result["gate"], json!("allow"));
        assert_eq!(env.result["drift_class"], json!("none"));
        // an unlabelled (manifest-gap) endpoint, no breach → escalate
        let gap = edges(&[("vendor/x", "src/orchestrator")]); // vendor/x has no glob
        let env2 = make_gate_envelope("layer_drift", "s", &gap, &label_from_manifest(&gap, &m), &m);
        assert_eq!(env2.result["gate"], json!("escalate"));
        assert!(env2.coverage < 1.0);
    }

    /// Offline gate-validation bench (Phase 4b). For each repo in
    /// `QONTINUI_BENCH_REPOS`, loads the REAL `<repo>/.qontinui/layers.toml`, builds
    /// the REAL resolved edges, and runs the REAL gate path — printing breaches,
    /// cycles, the gate recommendation, and coverage over the COMPLETE edge set (the
    /// confirmation the Appendix-A hand re-classification was one-sided about).
    /// Run: `QONTINUI_BENCH_REPOS=...coord,...runner,...web cargo test --bin qontinui-runner layer_gate_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn layer_gate_bench() {
        let repos = std::env::var("QONTINUI_BENCH_REPOS").unwrap_or_default();
        if repos.trim().is_empty() {
            eprintln!("set QONTINUI_BENCH_REPOS=abs1,abs2,...");
            return;
        }
        for repo in repos.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let dir = std::path::Path::new(repo);
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or(repo);
            let manifest = match LayerManifest::load(dir) {
                Some(m) => m,
                None => {
                    println!("=== {name}: NO .qontinui/layers.toml (skipped) ===");
                    continue;
                }
            };
            let graph = crate::workflow_generation::code_graph::CodeGraph::build(dir);
            let edges = crate::mcp::code_semantics::module_graph::cross_module_edges(&graph);
            let layer_of = label_from_manifest(&edges, &manifest);
            let env = make_gate_envelope("layer_drift", name, &edges, &layer_of, &manifest);
            let r = &env.result;
            println!("=== REPO {name} ===");
            println!(
                "total_edges={} judged={} coverage={:.3} gate={} drift_class={} breaches={} cycles={} carve_out={}",
                r["total_edges"], r["judged_edges"], env.coverage, r["gate"], r["drift_class"],
                r["breaches"].as_array().map(|a| a.len()).unwrap_or(0),
                r["cycles"].as_array().map(|a| a.len()).unwrap_or(0),
                r["carve_out"],
            );
            for b in r["breaches"].as_array().into_iter().flatten() {
                println!(
                    "BREACH  {} [{}] -> {} [{}]",
                    b["from"], b["from_layer"], b["to"], b["to_layer"]
                );
            }
            for c in r["cycles"].as_array().into_iter().flatten() {
                println!("CYCLE   {:?}", c["members"]);
            }
            let unl = r["unlabelled_modules"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            if unl > 0 {
                println!(
                    "MANIFEST GAPS (unlabelled in-scope modules): {unl} — {:?}",
                    r["unlabelled_modules"]
                );
            }
        }
    }
}
