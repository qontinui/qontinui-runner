//! Route derivation (Fork C) — **all** route method/path comes from the shared
//! [`qontinui_types::endpoint_for::endpoint_for`]. This crate never re-derives paths;
//! it only carries the derived [`Endpoint`] into emitted FastAPI source and the
//! `openapi.json` aid. This is the mechanism by which the backend generator (#2) and
//! the app generator (#1) agree on the API surface without a runtime handoff.

use qontinui_types::endpoint_for::{endpoint_for, Endpoint};
use qontinui_types::functional_spec::Operation;
use qontinui_types::priorities_profile::Profile;

/// An operation bound to its derived HTTP endpoint. The `endpoint` is produced
/// **only** by [`endpoint_for`] — there is no second path-derivation path in this
/// crate, which is what guarantees #1↔#2 agreement.
#[derive(Debug, Clone)]
pub struct RouteBinding {
    /// The spec operation this route serves.
    pub operation: Operation,
    /// The derived `{method, path}` (from the shared `endpoint_for`).
    pub endpoint: Endpoint,
}

/// Bind one operation to its derived endpoint via the shared rule.
pub fn route_for(op: &Operation, profile: &Profile) -> RouteBinding {
    RouteBinding {
        operation: op.clone(),
        endpoint: endpoint_for(op, profile),
    }
}

/// Emit a minimal OpenAPI 3.1 document describing the bound routes. This is a
/// **derived verification aid** (Fork C / reconciliation §1) — NOT a contract #1
/// consumes; #1 derives the same endpoints from the shared `endpoint_for`. Returned
/// as a `serde_json::Value` so the caller can serialize deterministically.
pub fn openapi_document(routes: &[RouteBinding], title: &str) -> serde_json::Value {
    let mut paths = serde_json::Map::new();
    for r in routes {
        let method = r.endpoint.method.to_ascii_lowercase();
        let op_obj = serde_json::json!({
            "operationId": r.operation.name,
            "summary": format!("{} ({})", r.operation.name, r.operation.verb),
            "responses": {
                "201": { "description": "Created" }
            }
        });
        let entry = paths
            .entry(r.endpoint.path.clone())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(map) = entry {
            map.insert(method, op_obj);
        }
    }
    serde_json::json!({
        "openapi": "3.1.0",
        "info": { "title": title, "version": "0" },
        "paths": serde_json::Value::Object(paths)
    })
}
