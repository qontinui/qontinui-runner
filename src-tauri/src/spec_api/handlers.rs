//! Axum handlers for the Spec API. Endpoints under `/spec/...`.
//!
//! Surface (verified against the Section 2 prompt):
//! - `GET  /spec/get?path=<rel>`       — raw file contents (path-traversal protected)
//! - `GET  /spec/page/:id`             — bundled-page projection
//! - `GET  /spec/graph`                — cross-page graph (IDs only)
//! - `POST /spec/query`                — find* queries against IR
//! - `POST /spec/derive`               — derived data without persistence
//! - `GET  /spec/diff?since=<epoch>`   — pages whose IR/projection mtime is newer
//! - `POST /spec/author`               — write IR + regenerate projection + emit event
//! - `GET  /spec/subscribe`            — SSE stream of `spec.changed` events
//!
//! Every empty/error response carries a `reason` field per Section 2's
//! "no silent empty responses" constraint.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;

use crate::mcp::types::ApiState;

use super::distinctness;
use super::events::{self, SpecChanged};
use super::projection::project_ir_to_bundled_page;
use super::responses::{EmptyOk, QueryResult, SpecError};
use super::storage::{self, ReadWithinRootError};
use super::types::IrPageSpec;
use qontinui_types::spec_check::SpecValidation;

/// Feature flag for `POST /spec/derive`: when set, the endpoint hard-stops
/// on distinctness violations. Default off for the first 30 days post-ship
/// per Plan 04 Step 5 — gives the corpus cleanup pass (Path A) runway.
fn enforce_distinctness_on_derive() -> bool {
    std::env::var("QONTINUI_SPEC_DISTINCTNESS_ENFORCE_ON_DERIVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// GET /spec/get?path=<rel>
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GetQuery {
    pub path: Option<String>,
}

pub async fn get_file(State(_state): State<Arc<ApiState>>, Query(q): Query<GetQuery>) -> Response {
    let rel = match q.path.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SpecError::new("missing-path-query-param")),
            )
                .into_response();
        }
    };

    let root = storage::resolve_specs_root();
    match storage::read_within_root(&root, rel) {
        Ok(bytes) => {
            // Always respond as application/octet-stream — caller can
            // override via `Accept` later. Avoids guessing wrong MIME.
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/octet-stream")],
                bytes,
            )
                .into_response()
        }
        Err(ReadWithinRootError::OutsideRoot) => (
            StatusCode::FORBIDDEN,
            Json(SpecError::with_detail(
                "path-outside-root",
                json!({ "path": rel }),
            )),
        )
            .into_response(),
        Err(ReadWithinRootError::FileNotFound) => (
            StatusCode::NOT_FOUND,
            Json(SpecError::with_detail(
                "file-not-found",
                json!({ "path": rel }),
            )),
        )
            .into_response(),
        Err(ReadWithinRootError::RootMissing) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "specs-root-missing",
                json!({ "root": root.display().to_string() }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /spec/page/:id
// ---------------------------------------------------------------------------

pub async fn get_page(State(_state): State<Arc<ApiState>>, AxPath(id): AxPath<String>) -> Response {
    let root = storage::resolve_specs_root();
    match storage::read_projection(&root, &id) {
        Ok(Some(value)) => (StatusCode::OK, Json(value)).into_response(),
        Ok(None) => {
            // Fall back to projecting on-the-fly if the IR exists but
            // projection doesn't. Keeps the API useful when an IR file is
            // hand-edited without re-running the generator script.
            match storage::read_ir(&root, &id) {
                Ok(Some(doc)) => {
                    let notes = storage::read_notes(&root, &id).unwrap_or(None);
                    let value = project_ir_to_bundled_page(&doc, notes.as_deref());
                    (StatusCode::OK, Json(value)).into_response()
                }
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(SpecError::with_detail(
                        "page-not-found",
                        json!({ "id": id }),
                    )),
                )
                    .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(SpecError::with_detail(
                        "page-read-failed",
                        json!({ "id": id, "error": e }),
                    )),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "projection-read-failed",
                json!({ "id": id, "error": e }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /spec/graph
// ---------------------------------------------------------------------------

pub async fn get_graph(State(_state): State<Arc<ApiState>>) -> Response {
    let root = storage::resolve_specs_root();
    let ids = match storage::list_pages(&root) {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "list-pages-failed",
                    json!({ "error": e.to_string() }),
                )),
            )
                .into_response();
        }
    };
    if ids.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "pages": [],
                "reason": "no-pages-registered"
            })),
        )
            .into_response();
    }
    let mut pages: Vec<Value> = Vec::with_capacity(ids.len());
    for id in &ids {
        let entry = match storage::read_ir(&root, id) {
            Ok(Some(doc)) => json!({
                "id": doc.id,
                "stateIds": doc.states.iter().map(|s| &s.id).collect::<Vec<_>>(),
                "transitionIds": doc.transitions.iter().map(|t| &t.id).collect::<Vec<_>>(),
            }),
            Ok(None) => json!({
                "id": id,
                "stateIds": [],
                "transitionIds": [],
                "reason": "no-ir-document"
            }),
            Err(e) => json!({
                "id": id,
                "error": e,
                "reason": "ir-read-failed"
            }),
        };
        pages.push(entry);
    }
    (StatusCode::OK, Json(json!({ "ok": true, "pages": pages }))).into_response()
}

// ---------------------------------------------------------------------------
// GET /spec/list
// ---------------------------------------------------------------------------

/// Per-page `DiscoveredSpec` listing.
///
/// Returns a `specs` array where each entry mirrors the TypeScript
/// `DiscoveredSpec` shape consumed by the runner / web prompt builders
/// (`qontinui-runner/src/lib/spec-prompt-builder.ts`,
/// `qontinui-web/frontend/src/lib/spec-prompt-builder.ts`):
///
/// ```json
/// {
///   "specId": "<page-dir-name>",
///   "appName": "Qontinui Runner",
///   "config": {
///     "version": "<projection.version>",
///     "description": "<projection.description>",
///     "groups": <projection.groups>,
///     "metadata": <projection.metadata-or-null>
///   }
/// }
/// ```
///
/// Per-page error policy: skip pages whose projection fails to load (no
/// IR + no projection + no embedded fallback). Rationale: `/spec/list` is
/// for discovery-time enumeration and downstream consumers can call
/// `/spec/page/:id` to get a per-page error envelope. Mixing well-formed
/// entries with error entries here would force every consumer to filter.
pub async fn get_list(State(_state): State<Arc<ApiState>>) -> Response {
    build_list_response(&storage::resolve_specs_root())
}

/// Inner implementation of `get_list` parameterized by the storage root so
/// tests can exercise the response-building logic without constructing an
/// `ApiState`.
pub(crate) fn build_list_response(root: &std::path::Path) -> Response {
    let ids = match storage::list_pages(root) {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "list-pages-failed",
                    json!({ "error": e.to_string() }),
                )),
            )
                .into_response();
        }
    };
    if ids.is_empty() {
        return (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "specs": [],
                "reason": "no-pages-registered"
            })),
        )
            .into_response();
    }

    let mut specs: Vec<Value> = Vec::with_capacity(ids.len());
    for id in &ids {
        // Mirror get_page's resolution: prefer the on-disk projection;
        // fall back to projecting the IR document on-the-fly when the
        // projection file is absent (or fails to parse).
        let projection: Option<Value> = match storage::read_projection(root, id) {
            Ok(Some(v)) => Some(v),
            _ => match storage::read_ir(root, id) {
                Ok(Some(doc)) => {
                    let notes = storage::read_notes(root, id).unwrap_or(None);
                    Some(project_ir_to_bundled_page(&doc, notes.as_deref()))
                }
                _ => None,
            },
        };
        let Some(projection) = projection else {
            continue;
        };
        // Plan 04 Step 7 — surface the landed `SpecValidation` rollup on each
        // entry when the IR fails distinctness. Clean specs omit the field
        // entirely so consumers don't pay a deserialization cost.
        let validation: Option<SpecValidation> = match storage::read_ir(root, id) {
            Ok(Some(doc)) => {
                let r = distinctness::report(&doc);
                if r.ok {
                    None
                } else {
                    Some(distinctness::to_spec_validation(id, &r))
                }
            }
            _ => None,
        };
        let mut entry = json!({
            "specId": id,
            "appName": "Qontinui Runner",
            "config": {
                "version": projection.get("version").cloned().unwrap_or(Value::Null),
                "description": projection.get("description").cloned().unwrap_or(Value::Null),
                "groups": projection.get("groups").cloned().unwrap_or(Value::Array(Vec::new())),
                "metadata": projection.get("metadata").cloned().unwrap_or(Value::Null),
            }
        });
        if let Some(v) = validation {
            entry["validation"] = serde_json::to_value(v).unwrap_or(Value::Null);
        }
        specs.push(entry);
    }

    (StatusCode::OK, Json(json!({ "ok": true, "specs": specs }))).into_response()
}

// ---------------------------------------------------------------------------
// POST /spec/query
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryBody {
    pub kind: String,
    pub value: String,
}

pub async fn post_query(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<QueryBody>,
) -> Response {
    let root = storage::resolve_specs_root();
    let ids = match storage::list_pages(&root) {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "list-pages-failed",
                    json!({ "error": e.to_string() }),
                )),
            )
                .into_response();
        }
    };

    let mut docs: Vec<IrPageSpec> = Vec::new();
    for id in &ids {
        if let Ok(Some(doc)) = storage::read_ir(&root, id) {
            docs.push(doc);
        }
    }

    match body.kind.as_str() {
        "findStatesByGroup" => {
            let mut matches: Vec<Value> = Vec::new();
            for doc in &docs {
                for s in &doc.states {
                    if s.group.as_deref() == Some(body.value.as_str()) {
                        matches.push(json!({
                            "pageId": doc.id,
                            "stateId": s.id,
                            "name": s.name,
                        }));
                    }
                }
            }
            let reason = if matches.is_empty() {
                "no-matches"
            } else {
                "matched-by-group"
            };
            (StatusCode::OK, Json(QueryResult::ok(matches, reason))).into_response()
        }
        "findTransitionsByEffect" => {
            let mut matches: Vec<Value> = Vec::new();
            for doc in &docs {
                for t in &doc.transitions {
                    if t.effect.as_deref() == Some(body.value.as_str()) {
                        matches.push(json!({
                            "pageId": doc.id,
                            "transitionId": t.id,
                            "name": t.name,
                        }));
                    }
                }
            }
            let reason = if matches.is_empty() {
                "no-matches"
            } else {
                "matched-by-effect"
            };
            (StatusCode::OK, Json(QueryResult::ok(matches, reason))).into_response()
        }
        "findStatesReferencing" => {
            // States referenced by transitions whose activate/exit/from set
            // contains `value`.
            let mut matches: Vec<Value> = Vec::new();
            let needle = &body.value;
            for doc in &docs {
                for t in &doc.transitions {
                    let referenced = t.from_states.iter().any(|s| s == needle)
                        || t.activate_states.iter().any(|s| s == needle)
                        || t.exit_states
                            .as_deref()
                            .map(|v| v.iter().any(|s| s == needle))
                            .unwrap_or(false);
                    if referenced {
                        matches.push(json!({
                            "pageId": doc.id,
                            "transitionId": t.id,
                            "stateId": needle,
                        }));
                    }
                }
            }
            let reason = if matches.is_empty() {
                "no-matches"
            } else {
                "matched-references"
            };
            (StatusCode::OK, Json(QueryResult::ok(matches, reason))).into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(SpecError::with_detail(
                "unknown-query-kind",
                json!({ "kind": other }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// POST /spec/derive
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeriveBody {
    pub page_id: String,
    pub derivation: String,
}

pub async fn post_derive(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<DeriveBody>,
) -> Response {
    build_derive_response(
        &storage::resolve_specs_root(),
        body,
        enforce_distinctness_on_derive(),
    )
}

/// Inner implementation of `post_derive` parameterized by the storage root
/// and the distinctness-enforce toggle (tests pass the flag explicitly to
/// avoid env-var races under cargo's parallel test runner).
pub(crate) fn build_derive_response(
    root: &std::path::Path,
    body: DeriveBody,
    enforce_distinctness: bool,
) -> Response {
    let doc = match storage::read_ir(root, &body.page_id) {
        Ok(Some(d)) => d,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(SpecError::with_detail(
                    "page-not-found",
                    json!({ "pageId": body.page_id }),
                )),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "ir-read-failed",
                    json!({ "pageId": body.page_id, "error": e }),
                )),
            )
                .into_response();
        }
    };

    // Plan 04 Step 5 — feature-flagged distinctness enforcement on derive.
    // Default off; flipped to on after the corpus cleanup pass (Step 9) lands.
    if enforce_distinctness {
        if let Err(report) = distinctness::validate(&doc) {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(SpecError::with_detail(
                    "spec-validation-failed",
                    json!({
                        "pageId": body.page_id,
                        "violations": report.violations,
                    }),
                )),
            )
                .into_response();
        }
    }

    match body.derivation.as_str() {
        "incomingTransitions" => {
            // Map state-id -> Vec<transition-id> activating it.
            let mut by_state: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for t in &doc.transitions {
                for activated in &t.activate_states {
                    by_state
                        .entry(activated.clone())
                        .or_default()
                        .push(t.id.clone());
                }
            }
            let result: Vec<Value> = doc
                .states
                .iter()
                .map(|s| {
                    json!({
                        "stateId": s.id,
                        "incomingTransitions": by_state
                            .get(&s.id)
                            .cloned()
                            .unwrap_or_default()
                    })
                })
                .collect();
            (
                StatusCode::OK,
                Json(json!({
                    "ok": true,
                    "pageId": body.page_id,
                    "derivation": body.derivation,
                    "result": result,
                })),
            )
                .into_response()
        }
        other => (
            StatusCode::BAD_REQUEST,
            Json(SpecError::with_detail(
                "unknown-derivation",
                json!({ "derivation": other }),
            )),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// GET /spec/diff?since=<epoch-secs>
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub since: Option<u64>,
}

pub async fn get_diff(State(_state): State<Arc<ApiState>>, Query(q): Query<DiffQuery>) -> Response {
    let since = q.since.unwrap_or(0);
    let root = storage::resolve_specs_root();
    let ids = match storage::list_pages(&root) {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "list-pages-failed",
                    json!({ "error": e.to_string() }),
                )),
            )
                .into_response();
        }
    };
    let mut changed: Vec<Value> = Vec::new();
    for id in &ids {
        if let Some(mtime) = storage::newest_mtime_for_page(&root, id) {
            if mtime > since {
                changed.push(json!({ "pageId": id, "mtime": mtime }));
            }
        }
    }
    let reason = if changed.is_empty() {
        "no-changes-since"
    } else {
        "changes-detected"
    };
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "since": since,
            "changed": changed,
            "reason": reason,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /spec/author
// ---------------------------------------------------------------------------

pub async fn post_author(
    State(_state): State<Arc<ApiState>>,
    Json(doc): Json<IrPageSpec>,
) -> Response {
    build_author_response(&storage::resolve_specs_root(), doc)
}

/// Inner implementation of `post_author` parameterized by the storage root
/// so tests can exercise the validation + write path without constructing
/// an `ApiState`.
pub(crate) fn build_author_response(root: &std::path::Path, doc: IrPageSpec) -> Response {
    if doc.id.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(SpecError::new("missing-document-id")),
        )
            .into_response();
    }
    if doc.version != "1.0" {
        return (
            StatusCode::BAD_REQUEST,
            Json(SpecError::with_detail(
                "unsupported-version",
                json!({ "got": doc.version, "expected": "1.0" }),
            )),
        )
            .into_response();
    }
    // Plan 04 Step 5 — hard-stop on distinctness violations. 422 per RFC 4918
    // §11.2 (semantic rejection, not a parse failure).
    if let Err(report) = distinctness::validate(&doc) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(SpecError::with_detail(
                "spec-validation-failed",
                json!({
                    "pageId": doc.id,
                    "violations": report.violations,
                }),
            )),
        )
            .into_response();
    }
    if let Err(e) = std::fs::create_dir_all(root) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SpecError::with_detail(
                "specs-root-create-failed",
                json!({ "error": e.to_string() }),
            )),
        )
            .into_response();
    }
    let projection_path = match storage::write_ir_and_regenerate(root, &doc) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpecError::with_detail(
                    "write-failed",
                    json!({ "error": e }),
                )),
            )
                .into_response();
        }
    };
    events::emit(events::SpecApiEvent::SpecChanged(SpecChanged {
        page_id: doc.id.clone(),
        kind: "ir-and-projection".to_string(),
        at_ms: events::now_ms(),
    }));
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "pageId": doc.id,
            "projectionPath": projection_path.display().to_string(),
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /spec/subscribe
// ---------------------------------------------------------------------------

pub async fn get_subscribe(
    State(_state): State<Arc<ApiState>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = events::subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| async move {
        match res {
            Ok(ev) => {
                // Use the variant's kebab-case discriminator (matches the
                // `#[serde(tag = "type", rename_all = "kebab-case")]` shape)
                // as the SSE event name.
                let event_name = match &ev {
                    events::SpecApiEvent::SpecChanged(_) => "spec-changed",
                    events::SpecApiEvent::SpecCheckInvoked { .. } => "spec-check-invoked",
                    events::SpecApiEvent::SpecCheckCompleted { .. } => "spec-check-completed",
                    events::SpecApiEvent::SpecCheckPolicyViolation { .. } => {
                        "spec-check-policy-violation"
                    }
                    events::SpecApiEvent::FlywheelProposalPromoted { .. } => {
                        "flywheel-proposal-promoted"
                    }
                    events::SpecApiEvent::FlywheelProposalDemoted { .. } => {
                        "flywheel-proposal-demoted"
                    }
                };
                match Event::default().event(event_name).json_data(&ev) {
                    Ok(e) => Some(Ok::<Event, Infallible>(e)),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
}

/// Smoke endpoint — used to confirm the spec_api router is mounted.
pub async fn get_health() -> Response {
    (StatusCode::OK, Json(EmptyOk::new("spec-api-mounted"))).into_response()
}

// ---------------------------------------------------------------------------
// POST /spec/validate  (Plan 04 Step 6)
// ---------------------------------------------------------------------------

/// Diagnostic body: caller supplies either an inline `document` or a `pageId`
/// pointing at an on-disk IR. The endpoint always returns 200 OK with the
/// full `DistinctnessReport` (the 422 hard-stop contract is reserved for
/// persistence endpoints — `/spec/validate` is a read-only diagnostic).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateBody {
    #[serde(default)]
    pub document: Option<IrPageSpec>,
    #[serde(default)]
    pub page_id: Option<String>,
}

pub async fn post_validate(
    State(_state): State<Arc<ApiState>>,
    Json(body): Json<ValidateBody>,
) -> Response {
    build_validate_response(&storage::resolve_specs_root(), body)
}

/// Inner implementation of `post_validate` parameterized by the storage root.
pub(crate) fn build_validate_response(root: &std::path::Path, body: ValidateBody) -> Response {
    let doc: IrPageSpec = match (body.document, body.page_id.as_deref()) {
        (Some(d), _) => d,
        (None, Some(id)) => match storage::read_ir(root, id) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(SpecError::with_detail(
                        "page-not-found",
                        json!({ "pageId": id }),
                    )),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(SpecError::with_detail(
                        "ir-read-failed",
                        json!({ "pageId": id, "error": e }),
                    )),
                )
                    .into_response();
            }
        },
        (None, None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SpecError::new("missing-document-or-page-id")),
            )
                .into_response();
        }
    };
    let report = distinctness::report(&doc);
    (StatusCode::OK, Json(report)).into_response()
}
