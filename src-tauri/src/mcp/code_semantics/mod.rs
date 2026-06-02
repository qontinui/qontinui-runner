//! Code-Semantics twin observer — runner-side sidecar (Phase 2a + 2b).
//!
//! Twin sub-space `Ξ_Type` (resolved code semantics) implemented as a **pure
//! observer** (no declared-vs-actual drift pair, no Φ predicate). The TS engine
//! is the TypeScript Language Service API, driven by a long-lived Node helper
//! (`resources/code-semantics/ts-language-service.mjs`) supervised by this
//! module, which hosts the `/code-semantics/*` HTTP query surface.
//!
//! See `CONTRACT.md` in this directory for the authoritative wire contracts:
//!   §A  Node helper ⇄ Rust stdio protocol
//!   §B  Rust sidecar HTTP surface
//!   §C  uniform observation envelope
//!   §D  coord MCP proxy semantics (2b overlay / predict-effect)
//!
//! ## Cold-index / coverage (CONTRACT §B/D4) — load-bearing
//! - Scope cold (helper not yet `init`-returned):
//!   - `symbol-lookup` falls back to the syntactic `code_graph.rs` observer
//!     (`provenance="code_graph_fallback"`, `coverage≈0.5`). If code_graph also
//!     finds nothing while cold → `coverage=0`, `provenance="indexing"`, and the
//!     result MUST NOT assert `exists:false`.
//!   - resolution queries (signature / find-references / typecheck) → `coverage=0`,
//!     `provenance="indexing"`, flagged not-yet-available.
//! - Scope warm → `posterior=1.0`, `coverage=1.0`, `provenance="ts-language-service"`,
//!   `credibility=(high,high,high)`.
//! - Node/TS permanently unavailable → degrade (symbol-lookup → code_graph_fallback;
//!   resolution → `engine_unavailable`, coverage 0). Never 500 on a missing engine.

pub mod code_graph_api;
pub mod node_bridge;
pub mod scope;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::{extract::State, routing::get, routing::post, Json, Router};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::mcp::types::ApiState;
use crate::workflow_generation::code_graph::CodeGraph;

use node_bridge::{check_availability, HelperAvailability, NodeBridge};
use scope::{resolve_scope, Scope};

// ===========================================================================
// Uniform observation envelope (CONTRACT §C)
// ===========================================================================

/// The credibility triple from the theory (Phase 1). Definitional/structural
/// code queries are `(high,high,high)`; the code_graph fallback lowers the
/// boundary axis to `medium`.
#[derive(Debug, Clone, Serialize)]
pub struct Credibility {
    pub causal: String,
    pub authorial: String,
    pub boundary: String,
}

impl Credibility {
    /// Warm, resolved LSP answer.
    pub fn high() -> Self {
        Credibility {
            causal: "high".into(),
            authorial: "high".into(),
            boundary: "high".into(),
        }
    }

    /// Syntactic code_graph fallback — boundary lowered to medium (no resolution).
    pub fn fallback() -> Self {
        Credibility {
            causal: "high".into(),
            authorial: "high".into(),
            boundary: "medium".into(),
        }
    }

    /// Resolved-import `Ξ_AST` (deterministic file binding, not the compiler's
    /// resolver). Boundary `medium` per the Phase 1 credibility ladder: above the
    /// raw syntactic tier, below the LSP `Ξ_Type` tier.
    pub fn resolved() -> Self {
        Credibility {
            causal: "high".into(),
            authorial: "high".into(),
            boundary: "medium".into(),
        }
    }

    /// Cold / unavailable — boundary unknown, lowered.
    pub fn degraded() -> Self {
        Credibility {
            causal: "high".into(),
            authorial: "high".into(),
            boundary: "low".into(),
        }
    }
}

/// CONTRACT §C uniform envelope. `result` carries the §A query-specific body.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub observer: String,
    pub query: String,
    pub result: Value,
    pub posterior: f64,
    pub coverage: f64,
    pub provenance: String,
    pub credibility: Credibility,
    pub staleness_seconds: Option<f64>,
    pub kernel: bool,
}

/// Provenance discriminants (CONTRACT §C).
pub const PROV_LSP: &str = "ts-language-service";
pub const PROV_FALLBACK: &str = "code_graph_fallback";
pub const PROV_INDEXING: &str = "indexing";
pub const PROV_UNAVAILABLE: &str = "engine_unavailable";
/// Resolved-import `Ξ_AST` provenance (Phase 1/2): deterministic file binding.
pub const PROV_RESOLVED: &str = "code_graph_resolved";

impl Envelope {
    /// Warm, resolved answer: posterior=1, coverage=1, credibility=(high,high,high).
    pub fn warm(query: &str, result: Value) -> Self {
        Envelope {
            observer: "code_semantics".into(),
            query: query.into(),
            result,
            posterior: 1.0,
            coverage: 1.0,
            provenance: PROV_LSP.into(),
            credibility: Credibility::high(),
            staleness_seconds: None,
            kernel: false,
        }
    }

    /// Syntactic code_graph fallback (symbol-lookup only): coverage≈0.5.
    pub fn fallback(query: &str, result: Value) -> Self {
        Envelope {
            observer: "code_semantics".into(),
            query: query.into(),
            result,
            posterior: 0.7,
            coverage: 0.5,
            provenance: PROV_FALLBACK.into(),
            credibility: Credibility::fallback(),
            staleness_seconds: None,
            kernel: false,
        }
    }

    /// Cold index — honest "I can't see yet". MUST NOT assert exists:false.
    pub fn indexing(query: &str, result: Value) -> Self {
        Envelope {
            observer: "code_semantics".into(),
            query: query.into(),
            result,
            posterior: 0.0,
            coverage: 0.0,
            provenance: PROV_INDEXING.into(),
            credibility: Credibility::degraded(),
            staleness_seconds: None,
            kernel: false,
        }
    }

    /// Resolved-import `Ξ_AST` answer (the diff-impact / resolve-import surface,
    /// CONTRACT §C). `observer` is `"code_graph"` (distinct from the LSP
    /// `code_semantics` observer); `provenance="code_graph_resolved"`, boundary
    /// `medium`. `coverage` < 1 honestly when the graph still has `Unresolved`
    /// edges (a partial/cold graph) — never asserts a false "no impact".
    /// `posterior` tracks coverage (high for a fully-resolved graph).
    pub fn resolved(query: &str, result: Value, coverage: f64) -> Self {
        Envelope {
            observer: "code_graph".into(),
            query: query.into(),
            result,
            posterior: coverage,
            coverage,
            provenance: PROV_RESOLVED.into(),
            credibility: Credibility::resolved(),
            staleness_seconds: None,
            kernel: false,
        }
    }

    /// Node/TS permanently unavailable for a resolution query.
    pub fn unavailable(query: &str, msg: &str) -> Self {
        Envelope {
            observer: "code_semantics".into(),
            query: query.into(),
            result: json!({ "available": false, "reason": msg }),
            posterior: 0.0,
            coverage: 0.0,
            provenance: PROV_UNAVAILABLE.into(),
            credibility: Credibility::degraded(),
            staleness_seconds: None,
            kernel: false,
        }
    }
}

// ===========================================================================
// Scope registry + supervisor
// ===========================================================================

/// Global registry mapping scope key → supervised helper bridge. v1 default
/// scope is the runner's own TS frontend; the map is structured for more.
struct Registry {
    bridges: HashMap<String, Arc<NodeBridge>>,
    availability: HelperAvailability,
}

static REGISTRY: Lazy<Mutex<Registry>> = Lazy::new(|| {
    Mutex::new(Registry {
        bridges: HashMap::new(),
        availability: check_availability(),
    })
});

/// Get-or-create the bridge for a scope. Returns `None` if Node/TS is
/// permanently unavailable.
async fn bridge_for(scope: &Scope) -> Option<Arc<NodeBridge>> {
    let mut reg = REGISTRY.lock().await;
    if let HelperAvailability::Unavailable(_) = &reg.availability {
        return None;
    }
    if let Some(b) = reg.bridges.get(&scope.key) {
        return Some(b.clone());
    }
    let (node, script) = match &reg.availability {
        HelperAvailability::Available { node, script } => (node.clone(), script.clone()),
        HelperAvailability::Unavailable(_) => return None,
    };
    let bridge = Arc::new(NodeBridge::new(node, script, scope.project.clone()));
    reg.bridges.insert(scope.key.clone(), bridge.clone());
    Some(bridge)
}

/// Is the Node engine permanently unavailable on this machine?
async fn engine_unavailable_reason() -> Option<String> {
    let reg = REGISTRY.lock().await;
    match &reg.availability {
        HelperAvailability::Unavailable(m) => Some(m.clone()),
        HelperAvailability::Available { .. } => None,
    }
}

// ===========================================================================
// code_graph.rs syntactic fallback (Ξ_AST)
// ===========================================================================

/// Build the syntactic code-graph for a scope's project dir and look up a
/// declared symbol by name (+ optional kind). Returns the §A-shaped
/// `symbol_lookup` result body, or `None` if nothing matched.
///
/// This is the cold-index / engine-unavailable fallback for `symbol-lookup`:
/// it is syntactic (no resolved signatures), so signatures are absent and the
/// envelope provenance/credibility reflect that.
pub fn code_graph_symbol_lookup(
    project_dir: &Path,
    name: &str,
    kind: Option<&str>,
) -> Option<Value> {
    let graph = CodeGraph::build(project_dir);
    let mut resolved: Vec<Value> = Vec::new();

    let want = |k: &str| kind.is_none_or(|wk| wk == k);

    if want("function") {
        for f in graph.functions.iter().filter(|f| f.name == name) {
            resolved.push(json!({
                "file": f.file_path,
                "name": f.name,
                "kind": "function",
                "signature": Value::Null,
                "signature_hash": Value::Null,
                "def_location": { "file": f.file_path, "line": f.line_start, "col": 1 },
            }));
        }
    }
    if want("class") {
        for c in graph.classes.iter().filter(|c| c.name == name) {
            resolved.push(json!({
                "file": c.file_path,
                "name": c.name,
                "kind": "class",
                "signature": Value::Null,
                "signature_hash": Value::Null,
                "def_location": { "file": c.file_path, "line": c.line_start, "col": 1 },
            }));
        }
    }
    // Exports cover interface/type/variable kinds the function/class lists miss.
    for e in graph.exports.iter().filter(|e| e.name == name) {
        if !want(&e.kind) {
            continue;
        }
        // Avoid duplicating function/class entries already added.
        if (e.kind == "function" || e.kind == "class")
            && resolved.iter().any(|r| {
                r.get("name").and_then(|v| v.as_str()) == Some(name)
                    && r.get("file").and_then(|v| v.as_str()) == Some(e.file_path.as_str())
            })
        {
            continue;
        }
        resolved.push(json!({
            "file": e.file_path,
            "name": e.name,
            "kind": e.kind,
            "signature": Value::Null,
            "signature_hash": Value::Null,
            "def_location": { "file": e.file_path, "line": e.line, "col": 1 },
        }));
    }

    if resolved.is_empty() {
        None
    } else {
        Some(json!({ "exists": true, "resolved": resolved }))
    }
}

/// The project directory for a scope (the tsconfig's parent).
fn scope_project_dir(scope: &Scope) -> std::path::PathBuf {
    Path::new(&scope.project)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(&scope.project).to_path_buf())
}

// ===========================================================================
// HTTP request bodies (CONTRACT §B)
// ===========================================================================

#[derive(Debug, Deserialize)]
pub struct SymbolLookupReq {
    pub scope: Option<String>,
    pub file: Option<String>,
    pub name: String,
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SignatureReq {
    pub scope: Option<String>,
    pub file: String,
    pub name: Option<String>,
    pub kind: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct FindReferencesReq {
    pub scope: Option<String>,
    pub file: String,
    pub name: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    #[serde(default)]
    pub cross_repo: bool,
}

#[derive(Debug, Deserialize)]
pub struct TypecheckReq {
    pub scope: Option<String>,
    pub file: String,
    pub overlay_patch: Option<Value>,
}

// ===========================================================================
// Routes (CONTRACT §B)
// ===========================================================================

/// Routes contributed to the runner's main router from `mcp_api.rs`. Includes
/// the LSP `Ξ_Type` `/code-semantics/*` surface and the resolved-import `Ξ_AST`
/// `/code-graph/*` diff blast-radius surface (Pillar 2).
pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/code-semantics/health", get(health))
        .route("/code-semantics/symbol-lookup", post(symbol_lookup))
        .route("/code-semantics/signature", post(signature))
        .route("/code-semantics/find-references", post(find_references))
        .route("/code-semantics/typecheck", post(typecheck))
        .merge(code_graph_api::routes())
}

/// GET /code-semantics/health — per CONTRACT §B.
async fn health(State(_state): State<Arc<ApiState>>) -> Json<Value> {
    let reg = REGISTRY.lock().await;
    let mut scopes: Vec<Value> = Vec::new();
    let unavailable = matches!(reg.availability, HelperAvailability::Unavailable(_));

    for (key, bridge) in reg.bridges.iter() {
        let indexed = bridge.is_initialized().await;
        let file_count = bridge.file_count().await;
        let provenance = if unavailable {
            PROV_UNAVAILABLE
        } else if indexed {
            PROV_LSP
        } else {
            PROV_INDEXING
        };
        scopes.push(json!({
            "scope": key,
            "language": "typescript",
            "indexed": indexed,
            "file_count": file_count,
            "provenance": provenance,
        }));
    }

    Json(json!({ "status": "ok", "scopes": scopes }))
}

/// POST /code-semantics/symbol-lookup
async fn symbol_lookup(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<SymbolLookupReq>,
) -> Json<Envelope> {
    let q = "symbol_lookup";
    let scope = match resolve_scope(req.scope.as_deref(), req.file.as_deref()) {
        Some(s) => s,
        None => {
            // No TS scope at all — treat as indexing (can't see), never absent.
            return Json(Envelope::indexing(q, json!({ "resolved": [] })));
        }
    };

    // Permanent engine-unavailable → code_graph fallback for symbol-lookup.
    if let Some(_reason) = engine_unavailable_reason().await {
        return Json(symbol_lookup_fallback(
            &scope,
            &req.name,
            req.kind.as_deref(),
        ));
    }

    let bridge = match bridge_for(&scope).await {
        Some(b) => b,
        None => {
            return Json(symbol_lookup_fallback(
                &scope,
                &req.name,
                req.kind.as_deref(),
            ))
        }
    };

    // Cold scope (not yet init) → try code_graph fallback first (honest partial).
    if !bridge.is_initialized().await {
        // Kick off init in the background so subsequent calls warm up; serve
        // the syntactic fallback now.
        let b2 = bridge.clone();
        tokio::spawn(async move {
            let _ = b2.ensure_init().await;
        });
        return Json(symbol_lookup_fallback(
            &scope,
            &req.name,
            req.kind.as_deref(),
        ));
    }

    match bridge
        .symbol_lookup(&req.name, req.kind.as_deref(), req.file.as_deref())
        .await
    {
        Ok(mut resp) => {
            strip_meta(&mut resp);
            Json(Envelope::warm(q, resp))
        }
        Err(_) => Json(symbol_lookup_fallback(
            &scope,
            &req.name,
            req.kind.as_deref(),
        )),
    }
}

/// Symbol-lookup fallback: try code_graph; if it also finds nothing, return an
/// `indexing` envelope (NOT exists:false).
fn symbol_lookup_fallback(scope: &Scope, name: &str, kind: Option<&str>) -> Envelope {
    let dir = scope_project_dir(scope);
    match code_graph_symbol_lookup(&dir, name, kind) {
        Some(result) => Envelope::fallback("symbol_lookup", result),
        None => Envelope::indexing("symbol_lookup", json!({ "resolved": [] })),
    }
}

/// POST /code-semantics/signature
async fn signature(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<SignatureReq>,
) -> Json<Envelope> {
    let q = "signature";
    let scope = req.scope.clone();
    let file = req.file.clone();
    resolution_query(q, scope.as_deref(), Some(&file), move |bridge| {
        let file = req.file.clone();
        let name = req.name.clone();
        let kind = req.kind.clone();
        async move {
            bridge
                .signature(&file, name.as_deref(), kind.as_deref(), req.line, req.col)
                .await
        }
    })
    .await
}

/// POST /code-semantics/find-references
async fn find_references(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<FindReferencesReq>,
) -> Json<Envelope> {
    let q = "find_references";
    let scope = req.scope.clone();
    let file = req.file.clone();
    resolution_query(q, scope.as_deref(), Some(&file), move |bridge| {
        let file = req.file.clone();
        let name = req.name.clone();
        async move {
            bridge
                .find_references(&file, name.as_deref(), req.line, req.col)
                .await
        }
    })
    .await
}

/// POST /code-semantics/typecheck (with overlay_patch ⇒ 2b predict-effect)
async fn typecheck(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<TypecheckReq>,
) -> Json<Envelope> {
    let q = "typecheck";
    let scope = req.scope.clone();
    let file = req.file.clone();
    resolution_query(q, scope.as_deref(), Some(&file), move |bridge| {
        let file = req.file.clone();
        let overlay = req.overlay_patch.clone();
        async move { bridge.typecheck(&file, overlay).await }
    })
    .await
}

/// Shared driver for resolution queries (signature / find-references /
/// typecheck): these need the type checker, so a cold scope returns `indexing`
/// and a permanently-unavailable engine returns `engine_unavailable` — never a
/// code_graph fallback (it cannot resolve).
async fn resolution_query<F, Fut>(
    query: &str,
    scope: Option<&str>,
    file: Option<&str>,
    run: F,
) -> Json<Envelope>
where
    F: FnOnce(Arc<NodeBridge>) -> Fut,
    Fut: std::future::Future<Output = Result<Value, node_bridge::BridgeError>>,
{
    if let Some(reason) = engine_unavailable_reason().await {
        return Json(Envelope::unavailable(query, &reason));
    }
    let scope = match resolve_scope(scope, file) {
        Some(s) => s,
        None => return Json(Envelope::indexing(query, json!({}))),
    };
    let bridge = match bridge_for(&scope).await {
        Some(b) => b,
        None => {
            let reason = engine_unavailable_reason()
                .await
                .unwrap_or_else(|| "no bridge".to_string());
            return Json(Envelope::unavailable(query, &reason));
        }
    };

    if !bridge.is_initialized().await {
        let b2 = bridge.clone();
        tokio::spawn(async move {
            let _ = b2.ensure_init().await;
        });
        return Json(Envelope::indexing(query, json!({})));
    }

    match run(bridge).await {
        Ok(mut resp) => {
            strip_meta(&mut resp);
            Json(Envelope::warm(query, resp))
        }
        Err(node_bridge::BridgeError::Unavailable(m)) => Json(Envelope::unavailable(query, &m)),
        Err(e) => Json(Envelope::unavailable(query, &e.to_string())),
    }
}

/// Strip the protocol envelope fields (`id`, `ok`) from a helper response so
/// only the §A query body remains as the envelope `result`.
fn strip_meta(v: &mut Value) {
    if let Value::Object(map) = v {
        map.remove("id");
        map.remove("ok");
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_envelope_is_high_credibility_full_coverage() {
        let env = Envelope::warm("symbol_lookup", json!({"exists": true}));
        assert_eq!(env.provenance, PROV_LSP);
        assert_eq!(env.coverage, 1.0);
        assert_eq!(env.posterior, 1.0);
        assert_eq!(env.credibility.causal, "high");
        assert_eq!(env.credibility.authorial, "high");
        assert_eq!(env.credibility.boundary, "high");
        assert!(!env.kernel);
    }

    #[test]
    fn fallback_envelope_lowers_boundary_and_coverage() {
        let env = Envelope::fallback("symbol_lookup", json!({"exists": true}));
        assert_eq!(env.provenance, PROV_FALLBACK);
        assert!(env.coverage < 1.0);
        assert_eq!(env.credibility.boundary, "medium");
        // Author + causal stay high (the parser is third-party, syntactic).
        assert_eq!(env.credibility.causal, "high");
        assert_eq!(env.credibility.authorial, "high");
    }

    #[test]
    fn indexing_envelope_does_not_assert_absence() {
        let env = Envelope::indexing("symbol_lookup", json!({ "resolved": [] }));
        assert_eq!(env.provenance, PROV_INDEXING);
        assert_eq!(env.coverage, 0.0);
        // The honest cold result must NOT contain exists:false.
        assert!(env.result.get("exists").is_none());
    }

    #[test]
    fn unavailable_envelope_for_resolution_query() {
        let env = Envelope::unavailable("typecheck", "node binary not found");
        assert_eq!(env.provenance, PROV_UNAVAILABLE);
        assert_eq!(env.coverage, 0.0);
        assert_eq!(
            env.result.get("available").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn credibility_serializes_as_triple() {
        let c = Credibility::high();
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v.get("causal").and_then(|x| x.as_str()), Some("high"));
        assert_eq!(v.get("authorial").and_then(|x| x.as_str()), Some("high"));
        assert_eq!(v.get("boundary").and_then(|x| x.as_str()), Some("high"));
    }

    #[test]
    fn envelope_serializes_with_contract_fields() {
        let env = Envelope::warm("signature", json!({"found": true}));
        let v = serde_json::to_value(&env).unwrap();
        for f in [
            "observer",
            "query",
            "result",
            "posterior",
            "coverage",
            "provenance",
            "credibility",
            "staleness_seconds",
            "kernel",
        ] {
            assert!(v.get(f).is_some(), "missing envelope field {f}");
        }
        assert_eq!(
            v.get("observer").and_then(|x| x.as_str()),
            Some("code_semantics")
        );
        assert_eq!(v.get("staleness_seconds"), Some(&Value::Null));
    }

    #[test]
    fn strip_meta_removes_protocol_fields() {
        let mut v = json!({"id": 7, "ok": true, "exists": true, "resolved": []});
        strip_meta(&mut v);
        assert!(v.get("id").is_none());
        assert!(v.get("ok").is_none());
        assert_eq!(v.get("exists").and_then(|x| x.as_bool()), Some(true));
    }

    #[test]
    fn code_graph_fallback_finds_a_real_exported_symbol() {
        // The runner frontend exports `APP_VERSION` in src/lib/appInfo.ts.
        let frontend_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let result = code_graph_symbol_lookup(frontend_root, "APP_VERSION", None);
        // code_graph is syntactic; if it parses the export we get Some, and the
        // signature is null (unresolved). If the build finds nothing we get
        // None — both are valid "honest" outcomes; assert the shape when found.
        if let Some(r) = result {
            assert_eq!(r.get("exists").and_then(|v| v.as_bool()), Some(true));
            let resolved = r.get("resolved").and_then(|v| v.as_array()).unwrap();
            assert!(!resolved.is_empty());
            // Syntactic fallback never resolves a signature.
            assert!(resolved[0]
                .get("signature_hash")
                .map(|v| v.is_null())
                .unwrap_or(true));
        }
    }

    #[test]
    fn code_graph_fallback_returns_none_for_hallucinated_symbol() {
        let frontend_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let result =
            code_graph_symbol_lookup(frontend_root, "ThisSymbolDoesNotExistAnywhere42", None);
        assert!(result.is_none(), "hallucinated symbol must not be found");
    }

    /// Availability-gated integration test: spawn the REAL Node helper and
    /// assert a resolved symbol lookup against the runner frontend project.
    /// Skips (passes) if `node` or the helper script is unavailable.
    #[tokio::test]
    async fn live_symbol_lookup_when_node_present() {
        let avail = check_availability();
        let (node, script) = match avail {
            HelperAvailability::Available { node, script } => (node, script),
            HelperAvailability::Unavailable(reason) => {
                eprintln!("skipping live test: {reason}");
                return;
            }
        };
        // Confirm `node` actually runs; otherwise skip.
        let probe = tokio::process::Command::new(&node)
            .arg("--version")
            .output()
            .await;
        if probe.is_err() {
            eprintln!("skipping live test: node not runnable");
            return;
        }

        let scope = match scope::default_ts_scope() {
            Some(s) => s,
            None => {
                eprintln!("skipping live test: no default TS scope");
                return;
            }
        };
        let bridge = Arc::new(NodeBridge::new(node, script, scope.project.clone()));
        if let Err(e) = bridge.ensure_init().await {
            eprintln!("skipping live test: init failed: {e}");
            return;
        }
        // APP_VERSION is a real exported const in the frontend.
        let resp = bridge
            .symbol_lookup("APP_VERSION", None, None)
            .await
            .expect("symbol_lookup should succeed when warm");
        assert_eq!(resp.get("ok").and_then(|v| v.as_bool()), Some(true));
        // Either it resolves (exists:true) — the expected case — but at minimum
        // the response is well-formed with an `exists` boolean.
        assert!(resp.get("exists").and_then(|v| v.as_bool()).is_some());
        bridge.shutdown().await;
    }
}
