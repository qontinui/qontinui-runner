//! `:9876/plan-library/*` — the agent-facing door onto the qontinui-web plan &
//! prompt library (plan `2026-08-10-plan-and-prompt-library-in-web` Phase 3).
//!
//! Phase 2's directory scan is the *backbone*: it captures every file that
//! exists on disk in a configured root. This door captures what a scan cannot
//! see — prompts written into ad-hoc worktree directories, and **the edges**,
//! which only the agent that ran the prompt→plan chain knows.
//!
//! | route | forwards to | gated |
//! |---|---|---|
//! | `POST /plan-library/artifacts` | `POST {web}/api/v1/plan-library` (+ edges) | **yes** |
//! | `POST /plan-library/links` | `POST {web}/api/v1/plan-library/{id}/edges` | **yes** |
//! | `GET /plan-library/search` | `GET {web}/api/v1/plan-library?…` | no |
//! | `GET /plan-library/candidates` | `GET {web}/api/v1/plan-library/candidates?…` | no |
//!
//! ## Why an HTTP route and not an MCP tool (D5)
//!
//! Agents in this fleet see exactly one MCP server: `coord-mcp`, proxied by the
//! runner as a strict allowlisted passthrough that injects no runner-local
//! tools. Adding an MCP tool would therefore mean adding a *coord* tool, which
//! re-couples coord to plans — the exact coupling D1 removed. The briefing
//! already advertises `http://127.0.0.1:{api_port}`.
//!
//! ## Identity: the organization is NEVER accepted from the request
//!
//! Every forward attaches the **runner-resolved device JWT**
//! ([`crate::auth::attach_device_auth`] — the `plan_workunit_adapter::push`
//! shape), and the web side derives `organization_id` from that principal. This
//! module deliberately mirrors the web request model in having no
//! `organization_id` field at all: a caller-supplied org on an unauthenticated
//! loopback surface is a scope-escalation bug, and the surest defence is to give
//! the request nowhere to put one. `session_id` IS accepted, because it is
//! provenance labelling with no authority attached — it can only ever make a
//! version-log line more informative.
//!
//! It is emphatically **not** modelled on `mcp::skills::sync_push`, whose
//! `auth_token` is a caller-supplied request field: on a surface with no auth
//! layer that would let any local process choose the bearer.
//!
//! ## Provenance
//!
//! Writes through this door are stamped `captured_by: "agent"` and
//! `kind_is_heuristic: false`. The `false` is deliberate and is the opposite of
//! Phase 2's scan: an agent naming a kind is *asserting* it, so the write
//! resolves by the exact `(org, kind, slug, source_repo)` key and **locks** the
//! kind against a later heuristic re-scan. A scan guesses; an agent knows.
//!
//! ## Capability flag
//!
//! The two write routes are gated by [`PLAN_LIBRARY_WRITE_FLAG`], off by
//! default — see its doc comment for why, and
//! [`mcp::ui_bridge::gated_flow`](crate::mcp::ui_bridge::gated_flow) for the
//! shipped pattern this follows. The reads stay ungated and *advertise* the
//! flag, so the requirement is discoverable rather than learned by taking a 403.

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::mcp::types::{api_error, ApiResponse, ApiState};
use qontinui_runner_lib::plan_workunit_adapter::body_push::ArtifactUpsert;

type ApiResult = Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)>;

/// `captured_by` for every write through this door.
const CAPTURED_BY_AGENT: &str = "agent";

/// Largest `source_path` file this door will read and forward. Bodies are
/// markdown documents; anything past this is a mistake (a log, a binary, a
/// generated dump) and shipping it into the corpus helps nobody.
const MAX_SOURCE_FILE_BYTES: u64 = 2 * 1024 * 1024;

// ===========================================================================
// Capability flag
// ===========================================================================

/// Capability flag gating the two **write** routes. **Off by default**;
/// enabled only on an exact `"1"`.
///
/// Two independent reasons, either of which alone would justify it:
///
/// 1. **The runner's `:9876` bridge binds `127.0.0.1` with no auth layer at
///    all** — localhost binding is the entire trust boundary. (`UI_BRIDGE_REQUIRE_AUTH`
///    gates the *qontinui-web relay*, not this server, so it is a false premise
///    here.) Without a flag, any local process could write into the operator's
///    plan corpus using the runner's own credential.
/// 2. **Without it the `plan_capture` fleet toggle would be advisory only.**
///    Phase 4 refuses to inject the write-door clause into the system prompt at
///    level `off`, on the principle that an instruction with no live
///    authorization must not appear in a system prompt — but the door would
///    still accept writes at `off`. The flag makes `off` mean the clause is
///    absent *and* the door is shut.
pub const PLAN_LIBRARY_WRITE_FLAG: &str = "QONTINUI_PLAN_LIBRARY_WRITE";

/// Whether the write door is enabled for this process.
///
/// Read **per request, never cached**, so an operator can flip it without
/// restarting the runner — the shipped `gated_flow::view_control_enabled`
/// contract. (Restarting a runner is forbidden by fleet policy, so a cached
/// read would make the flag effectively permanent for the process's life.)
pub fn write_enabled() -> bool {
    std::env::var(PLAN_LIBRARY_WRITE_FLAG)
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// The capability block the ungated read routes advertise.
///
/// Reads stay ungated for the same reason `GET /ui-bridge/views` does —
/// *"knowing a view exists is inert; opening it is the privileged act"* — and
/// because Phase 6's whole value is an agent being able to ask the candidate
/// question. But a driver that can only learn the write requirement by taking a
/// 403 has an escape hatch that is both off by default AND undiscoverable, so
/// every read answer carries the flag name and the exact instruction.
fn write_capability() -> serde_json::Map<String, Value> {
    let enabled = write_enabled();
    let instruction = if enabled {
        format!(
            "POST /plan-library/artifacts and POST /plan-library/links are ENABLED \
             ({PLAN_LIBRARY_WRITE_FLAG}=1)."
        )
    } else {
        format!(
            "POST /plan-library/artifacts and POST /plan-library/links are DISABLED and \
             will 403. Set {PLAN_LIBRARY_WRITE_FLAG}=1 in the runner's environment to \
             enable them (for a supervisor temp runner: spawn with extra_env: \
             {{\"{PLAN_LIBRARY_WRITE_FLAG}\": \"1\"}}). These read routes work regardless."
        )
    };
    let mut m = serde_json::Map::new();
    m.insert("writeEnabled".to_string(), Value::Bool(enabled));
    m.insert(
        "writeFlag".to_string(),
        Value::String(PLAN_LIBRARY_WRITE_FLAG.to_string()),
    );
    m.insert("writeInstruction".to_string(), Value::String(instruction));
    m
}

/// Fold [`write_capability`] into an upstream payload without disturbing its
/// own keys. A bare array is wrapped rather than lost.
fn with_write_capability(upstream: Value) -> Value {
    let cap = write_capability();
    match upstream {
        Value::Object(mut m) => {
            m.extend(cap);
            Value::Object(m)
        }
        other => {
            let mut m = cap;
            m.insert("result".to_string(), other);
            Value::Object(m)
        }
    }
}

/// The 403 the write routes answer when the flag is off. Names the flag and the
/// working alternative, so the refusal is self-explanatory.
fn write_disabled_error() -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::FORBIDDEN,
        Json(api_error(format!(
            "the plan-library write door is disabled. Set {PLAN_LIBRARY_WRITE_FLAG}=1 in the \
             runner's environment to enable POST /plan-library/artifacts and \
             POST /plan-library/links (off by default: this server binds 127.0.0.1 with no \
             auth layer, so the flag is the authorization). GET /plan-library/search and \
             GET /plan-library/candidates work regardless and advertise this flag."
        ))),
    )
}

// ===========================================================================
// Requests
// ===========================================================================

/// One provenance edge to create alongside an artifact write. Exactly one of
/// `to_id` / `from_id`, matching the web edge route's own rule.
#[derive(Debug, Clone, Deserialize)]
pub struct EdgeSpec {
    #[serde(default)]
    pub to_id: Option<String>,
    #[serde(default)]
    pub from_id: Option<String>,
    /// `produced_report | feeds | authored_plan | supersedes | depends_on`.
    pub relation: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /plan-library/artifacts` body.
///
/// **No `organization_id`** — see the module docs. `session_id` is optional
/// provenance only.
#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactWriteRequest {
    pub kind: String,
    pub slug: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Opaque. Never validated here or upstream.
    #[serde(default)]
    pub status: Option<String>,
    /// The markdown body. Supply this **or** `source_path`.
    #[serde(default)]
    pub body: Option<String>,
    /// A local file to read the body from — the case a scan cannot reach, e.g.
    /// a prompt the agent wrote into an ad-hoc worktree directory.
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub source_repo: Option<String>,
    #[serde(default)]
    pub work_unit_slug: Option<String>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub authored_at: Option<String>,
    #[serde(default)]
    pub change_description: Option<String>,
    /// The writing session, recorded on the version row so Phase 5 can show
    /// compliance without guessing. Provenance, never authorization.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Edges to create once the artifact has an id.
    #[serde(default)]
    pub edges: Vec<EdgeSpec>,
}

/// `POST /plan-library/links` body — one edge between two existing artifacts.
///
/// Flat and symmetric (`from_id` + `to_id`), unlike the web route's
/// anchor-relative shape, because an agent linking a prompt to the plan it
/// produced knows both ends and should not have to decide which is "the path
/// artifact".
#[derive(Debug, Clone, Deserialize)]
pub struct LinkRequest {
    pub from_id: String,
    pub to_id: String,
    pub relation: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

// ===========================================================================
// Upstream plumbing
// ===========================================================================

/// The qontinui-web base URL, trailing slash trimmed.
fn web_base() -> String {
    crate::api_config::get_api_base_url()
        .trim_end_matches('/')
        .to_string()
}

/// Turn a non-2xx upstream answer into this door's error, **verbatim**.
///
/// coord-style: the upstream status code and body pass through unchanged rather
/// than being remapped to a generic 500, so an agent sees the real 422 field
/// error, the real 409 kind conflict, and the real 404 id. The one thing added
/// is the 401/403 diagnosis, because a bare 401 here is systematically
/// misleading — it is far more likely to be the dependency mismatch below than
/// a genuinely unauthorized caller.
fn upstream_failure(
    what: &str,
    status: reqwest::StatusCode,
    text: String,
) -> (StatusCode, Json<ApiResponse<()>>) {
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut message = format!("{what}: upstream {status}: {text}");
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        message.push_str(
            "\nnote: this door presents the runner's coord-issued DEVICE JWT. The qontinui-web \
             plan-library routes must accept a device bearer \
             (app.api.deps.get_authenticated_device_user, or a user-or-device dependency \
             like get_audit_actor_user_id); a route wired to `current_active_user` alone is \
             Cognito-only and rejects it.",
        );
    }
    (code, Json(api_error(message)))
}

/// Transport failure (DNS, refused connection, timeout) — the backend is not
/// answering at all, which is a different thing from it answering "no".
fn transport_failure(what: &str, e: reqwest::Error) -> (StatusCode, Json<ApiResponse<()>>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(api_error(format!(
            "{what}: could not reach the qontinui-web backend at {}: {e}",
            web_base()
        ))),
    )
}

/// GET a web plan-library path, forwarding `params` as the query string.
async fn upstream_get(
    path: &str,
    params: &HashMap<String, String>,
) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
    let base = web_base();
    let mut url = format!("{base}{path}");
    if !params.is_empty() {
        let qs: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect();
        url.push('?');
        url.push_str(&qs.join("&"));
    }
    let client = reqwest::Client::new();
    let resp = crate::auth::attach_device_auth(client.get(&url))
        .send()
        .await
        .map_err(|e| transport_failure(path, e))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(upstream_failure(path, status, text));
    }
    serde_json::from_str(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("{path}: unparseable upstream body: {e}"))),
        )
    })
}

/// POST a JSON body to a web plan-library path.
async fn upstream_post(
    path: &str,
    body: &Value,
) -> Result<Value, (StatusCode, Json<ApiResponse<()>>)> {
    let url = format!("{}{}", web_base(), path);
    let client = reqwest::Client::new();
    let resp = crate::auth::attach_device_auth(client.post(&url).json(body))
        .send()
        .await
        .map_err(|e| transport_failure(path, e))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(upstream_failure(path, status, text));
    }
    serde_json::from_str(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("{path}: unparseable upstream body: {e}"))),
        )
    })
}

// ===========================================================================
// Pure request → payload mapping
// ===========================================================================

/// Compose the provenance note recorded on the artifact's version row.
///
/// The web upsert model has **no `session_id` field**, so the session travels
/// in `change_description` — the one field that lands on the append-only
/// version snapshot, which is exactly where "who changed this, and why" belongs.
/// A caller-supplied `change_description` is kept and the session appended, so
/// the agent's own note is never replaced by bookkeeping.
pub fn provenance_note(
    change_description: Option<&str>,
    session_id: Option<&str>,
) -> Option<String> {
    match (change_description, session_id) {
        (Some(d), Some(s)) => Some(format!("{d} (agent write, session {s})")),
        (Some(d), None) => Some(format!("{d} (agent write)")),
        (None, Some(s)) => Some(format!("agent write, session {s}")),
        (None, None) => Some("agent write".to_string()),
    }
}

/// Build the upstream upsert payload from a validated request + resolved body.
///
/// Pure, so the two properties that matter are unit tests rather than claims:
/// the payload carries no organization, and `kind_is_heuristic` is `false`.
pub fn build_agent_upsert(req: &ArtifactWriteRequest, body: String) -> ArtifactUpsert {
    ArtifactUpsert {
        kind: req.kind.clone(),
        slug: req.slug.clone(),
        title: req.title.clone().unwrap_or_default(),
        status: req.status.clone().unwrap_or_default(),
        // Let the server compute the digest. An agent-supplied body is not read
        // from a file we hashed, and a digest that disagrees with sha256(body)
        // is a hard 422 upstream — so sending none is strictly safer than
        // sending one we would have to keep in sync with any body rewrite here.
        content_sha256: None,
        source_path: req.source_path.clone(),
        source_repo: req.source_repo.clone(),
        work_unit_slug: req.work_unit_slug.clone(),
        repos: req.repos.clone(),
        authored_at: req.authored_at.clone(),
        captured_by: CAPTURED_BY_AGENT.to_string(),
        change_description: provenance_note(
            req.change_description.as_deref(),
            req.session_id.as_deref(),
        ),
        // An explicit kind from an agent is an ASSERTION, not a guess: resolve
        // by the exact identity and lock the kind so no later scan moves it.
        kind_is_heuristic: false,
        body,
    }
}

/// Resolve the artifact body: the literal `body`, else the contents of
/// `source_path`. Exactly one must be supplied.
fn resolve_body(req: &ArtifactWriteRequest) -> Result<String, String> {
    match (&req.body, &req.source_path) {
        (Some(b), _) => Ok(b.clone()),
        (None, Some(path)) => {
            let meta = std::fs::metadata(path)
                .map_err(|e| format!("cannot stat source_path {path:?}: {e}"))?;
            if !meta.is_file() {
                return Err(format!("source_path {path:?} is not a file"));
            }
            if meta.len() > MAX_SOURCE_FILE_BYTES {
                return Err(format!(
                    "source_path {path:?} is {} bytes, over the {MAX_SOURCE_FILE_BYTES}-byte \
                     limit for an artifact body",
                    meta.len()
                ));
            }
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read source_path {path:?}: {e}"))
        }
        (None, None) => Err(
            "supply either `body` or `source_path` (the file to read the body from)".to_string(),
        ),
    }
}

/// Validate one [`EdgeSpec`] and render it as the web edge route's
/// anchor-relative payload, returning `(anchor_id, body)`.
///
/// `default_anchor` is the artifact just written, used when the spec names only
/// the far end.
fn edge_payload(spec: &EdgeSpec, default_anchor: &str) -> Result<(String, Value), String> {
    match (&spec.to_id, &spec.from_id) {
        (Some(_), Some(_)) => Err(
            "supply exactly one of `to_id` (outgoing) or `from_id` (incoming) per edge".to_string(),
        ),
        (None, None) => Err(
            "supply exactly one of `to_id` (outgoing) or `from_id` (incoming) per edge".to_string(),
        ),
        (Some(to), None) => Ok((
            default_anchor.to_string(),
            serde_json::json!({"to_id": to, "relation": spec.relation, "note": spec.note}),
        )),
        (None, Some(from)) => Ok((
            default_anchor.to_string(),
            serde_json::json!({"from_id": from, "relation": spec.relation, "note": spec.note}),
        )),
    }
}

// ===========================================================================
// Handlers
// ===========================================================================

/// `POST /plan-library/artifacts` — write one artifact (and optionally its
/// edges) into the corpus. **Gated by [`PLAN_LIBRARY_WRITE_FLAG`].**
pub async fn write_artifact_handler(Json(req): Json<ArtifactWriteRequest>) -> ApiResult {
    if !write_enabled() {
        return Err(write_disabled_error());
    }
    if req.kind.trim().is_empty() || req.slug.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "`kind` and `slug` are required and must be non-empty",
            )),
        ));
    }
    let body = resolve_body(&req).map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;

    // Validate every edge BEFORE the upsert, so a malformed edge cannot leave a
    // half-written artifact behind: the artifact write is not transactional with
    // the edges, and the cheapest way to keep the two consistent is to refuse
    // the whole request up front.
    for spec in &req.edges {
        edge_payload(spec, "placeholder")
            .map_err(|e| (StatusCode::BAD_REQUEST, Json(api_error(e))))?;
    }

    let payload = serde_json::to_value(build_agent_upsert(&req, body)).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(api_error(format!("serialize upsert: {e}"))),
        )
    })?;
    let upstream = upstream_post("/api/v1/plan-library", &payload).await?;

    let artifact_id = upstream
        .get("artifact")
        .and_then(|a| a.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    // Edges, one call each — the web route takes one edge per request and is
    // idempotent on `(from_id, to_id, relation)`. Reported per edge rather than
    // aborting: the artifact already landed, and a caller needs to know WHICH
    // edge failed, not just that something did.
    let mut edge_results: Vec<Value> = Vec::new();
    for spec in &req.edges {
        let (anchor, edge_body) = match edge_payload(spec, &artifact_id) {
            Ok(v) => v,
            Err(e) => {
                edge_results.push(serde_json::json!({"ok": false, "error": e}));
                continue;
            }
        };
        let path = format!("/api/v1/plan-library/{anchor}/edges");
        match upstream_post(&path, &edge_body).await {
            Ok(v) => edge_results.push(serde_json::json!({"ok": true, "edge": v})),
            Err((code, err)) => edge_results.push(serde_json::json!({
                "ok": false,
                "status": code.as_u16(),
                "error": err.0.error,
            })),
        }
    }

    let mut out = upstream;
    if let Some(obj) = out.as_object_mut() {
        if !edge_results.is_empty() {
            obj.insert("edges".to_string(), Value::Array(edge_results));
        }
    }
    Ok(Json(ApiResponse::success(out)))
}

/// `POST /plan-library/links` — link two existing artifacts.
/// **Gated by [`PLAN_LIBRARY_WRITE_FLAG`].**
pub async fn create_link_handler(Json(req): Json<LinkRequest>) -> ApiResult {
    if !write_enabled() {
        return Err(write_disabled_error());
    }
    if req.from_id.trim().is_empty() || req.to_id.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error("`from_id` and `to_id` are required")),
        ));
    }
    if req.relation.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(api_error(
                "`relation` is required (produced_report | feeds | authored_plan | \
                 supersedes | depends_on)",
            )),
        ));
    }
    // Flat (from, to) → the web route's anchor-relative shape: anchor on
    // `from_id`, far end as `to_id`, which is the OUTGOING direction.
    let path = format!("/api/v1/plan-library/{}/edges", req.from_id);
    let body = serde_json::json!({
        "to_id": req.to_id,
        "relation": req.relation,
        "note": provenance_note(req.note.as_deref(), req.session_id.as_deref()),
    });
    let upstream = upstream_post(&path, &body).await?;
    Ok(Json(ApiResponse::success(upstream)))
}

/// Query parameters this door forwards to the web list route. Anything else is
/// dropped rather than passed through, so a typo cannot silently widen a read.
const SEARCH_PARAMS: [&str; 8] = [
    "q",
    "kind",
    "status",
    "repo",
    "since",
    "work_unit_slug",
    "offset",
    "limit",
];

/// `GET /plan-library/search` — full-text + filtered read over the corpus.
/// **Ungated**, and advertises the write flag.
pub async fn search_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    let forwarded: HashMap<String, String> = params
        .into_iter()
        .filter(|(k, _)| SEARCH_PARAMS.contains(&k.as_str()))
        .collect();
    let upstream = upstream_get("/api/v1/plan-library", &forwarded).await?;
    Ok(Json(ApiResponse::success(with_write_capability(upstream))))
}

/// Query parameters forwarded to the web candidates route.
const CANDIDATE_PARAMS: [&str; 3] = ["offset", "limit", "include_coord"];

/// `GET /plan-library/candidates` — unshipped plans with the ranking INPUTS
/// attached and **no score** (D6: the agent ranks). **Ungated**, and advertises
/// the write flag.
pub async fn candidates_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    let forwarded: HashMap<String, String> = params
        .into_iter()
        .filter(|(k, _)| CANDIDATE_PARAMS.contains(&k.as_str()))
        .collect();
    let upstream = upstream_get("/api/v1/plan-library/candidates", &forwarded).await?;
    Ok(Json(ApiResponse::success(with_write_capability(upstream))))
}

/// The door's route table, as data: `(method, path, gated_by_the_write_flag)`.
///
/// `Router` has no public introspection, so the gating split — the thing the
/// vet added this flag for — would otherwise be untestable. Keep in lockstep
/// with the `.route(` calls in [`routes`]; the test below pins the count.
pub fn route_entries() -> &'static [(&'static str, &'static str, bool)] {
    &[
        ("POST", "/plan-library/artifacts", true),
        ("POST", "/plan-library/links", true),
        ("GET", "/plan-library/search", false),
        ("GET", "/plan-library/candidates", false),
    ]
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/plan-library/artifacts", post(write_artifact_handler))
        .route("/plan-library/links", post(create_link_handler))
        .route("/plan-library/search", get(search_handler))
        .route("/plan-library/candidates", get(candidates_handler))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn req(kind: &str, slug: &str) -> ArtifactWriteRequest {
        ArtifactWriteRequest {
            kind: kind.to_string(),
            slug: slug.to_string(),
            title: Some("T".to_string()),
            status: Some("vetted".to_string()),
            body: Some("# T\n".to_string()),
            source_path: None,
            source_repo: Some("qontinui-dev-notes/prompts".to_string()),
            work_unit_slug: None,
            repos: vec!["qontinui-runner".to_string()],
            authored_at: None,
            change_description: None,
            session_id: Some("db54260f".to_string()),
            edges: Vec::new(),
        }
    }

    // ---- the flag ------------------------------------------------------

    /// Serialized against the other env-touching tests in this binary, and
    /// restored on the way out.
    fn with_flag<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[PLAN_LIBRARY_WRITE_FLAG]);
        match value {
            Some(v) => std::env::set_var(PLAN_LIBRARY_WRITE_FLAG, v),
            None => std::env::remove_var(PLAN_LIBRARY_WRITE_FLAG),
        }
        f()
    }

    /// Off by default is the whole point: an absent flag must not read as
    /// enabled, and neither must any of the plausible truthy spellings — the
    /// contract is an EXACT `"1"`.
    #[test]
    fn the_write_flag_is_off_unless_exactly_one() {
        assert!(!with_flag(None, write_enabled), "absent is off");
        assert!(!with_flag(Some(""), write_enabled));
        assert!(!with_flag(Some("0"), write_enabled));
        assert!(!with_flag(Some("true"), write_enabled));
        assert!(!with_flag(Some("yes"), write_enabled));
        assert!(!with_flag(Some(" 1"), write_enabled));
        assert!(!with_flag(Some("1 "), write_enabled));
        assert!(with_flag(Some("1"), write_enabled));
    }

    /// The reads advertise the requirement, so a driver never has to learn it
    /// by taking a 403.
    #[test]
    fn reads_advertise_the_flag_in_both_states() {
        let off = with_flag(None, || {
            with_write_capability(serde_json::json!({"items": []}))
        });
        assert_eq!(off["writeEnabled"], serde_json::json!(false));
        assert_eq!(off["writeFlag"], serde_json::json!(PLAN_LIBRARY_WRITE_FLAG));
        let msg = off["writeInstruction"].as_str().unwrap();
        assert!(msg.contains(PLAN_LIBRARY_WRITE_FLAG));
        assert!(msg.contains("403"));
        assert!(msg.contains("/plan-library/artifacts"));
        // The upstream payload's own keys survive.
        assert_eq!(off["items"], serde_json::json!([]));

        let on = with_flag(Some("1"), || with_write_capability(serde_json::json!({})));
        assert_eq!(on["writeEnabled"], serde_json::json!(true));
        assert!(on["writeInstruction"].as_str().unwrap().contains("ENABLED"));
    }

    #[test]
    fn a_bare_array_upstream_is_wrapped_not_lost() {
        let v = with_flag(None, || {
            with_write_capability(serde_json::json!([{"id": "a"}]))
        });
        assert_eq!(v["result"][0]["id"], serde_json::json!("a"));
        assert_eq!(v["writeEnabled"], serde_json::json!(false));
    }

    #[test]
    fn the_disabled_error_names_the_flag_and_the_working_alternative() {
        let (code, body) = write_disabled_error();
        assert_eq!(code, StatusCode::FORBIDDEN);
        let msg = body.0.error.clone().unwrap();
        assert!(msg.contains(PLAN_LIBRARY_WRITE_FLAG));
        assert!(msg.contains("/plan-library/search"));
    }

    // ---- the payload ---------------------------------------------------

    /// The security invariant: a caller-supplied organization is a
    /// scope-escalation bug, and the request model has nowhere to put one.
    #[test]
    fn the_upstream_payload_never_carries_an_organization() {
        let payload = build_agent_upsert(&req("plan", "s"), "# T\n".to_string());
        let j = serde_json::to_value(&payload).unwrap();
        let obj = j.as_object().unwrap();
        for forbidden in ["organization_id", "org_id", "organization", "tenant_id"] {
            assert!(
                !obj.contains_key(forbidden),
                "{forbidden} must never reach the wire"
            );
        }
    }

    /// A caller-supplied `organization_id` in the JSON body must be IGNORED,
    /// not honoured — the deserializer has no field for it, which is the
    /// property this asserts end to end.
    #[test]
    fn a_caller_supplied_organization_is_dropped_at_deserialization() {
        let raw = serde_json::json!({
            "kind": "plan",
            "slug": "s",
            "body": "# T\n",
            "organization_id": "00000000-0000-0000-0000-0000000000ff",
        });
        let parsed: ArtifactWriteRequest = serde_json::from_value(raw).unwrap();
        let j = serde_json::to_value(build_agent_upsert(&parsed, "# T\n".to_string())).unwrap();
        assert!(!j.as_object().unwrap().contains_key("organization_id"));
        assert!(
            !j.to_string().contains("0000000000ff"),
            "the supplied org must not survive anywhere in the payload"
        );
    }

    /// Agent writes assert the kind; scan writes guess it. Getting this
    /// backwards would let a later scan silently undo an agent's correction.
    #[test]
    fn agent_writes_are_stamped_agent_and_are_not_heuristic() {
        let payload = build_agent_upsert(&req("plan", "s"), "# T\n".to_string());
        assert_eq!(payload.captured_by, "agent");
        assert!(
            !payload.kind_is_heuristic,
            "an explicit kind from an agent must LOCK, not float"
        );
    }

    /// The digest is deliberately omitted: the server computes it, and a
    /// disagreeing one is a hard 422.
    #[test]
    fn the_agent_payload_omits_the_digest() {
        let payload = build_agent_upsert(&req("plan", "s"), "# T\n".to_string());
        assert_eq!(payload.content_sha256, None);
        let j = serde_json::to_value(&payload).unwrap();
        assert!(!j.as_object().unwrap().contains_key("content_sha256"));
    }

    #[test]
    fn the_session_id_rides_in_the_version_note_without_replacing_the_callers() {
        assert_eq!(
            provenance_note(None, Some("db54260f")).as_deref(),
            Some("agent write, session db54260f")
        );
        assert_eq!(
            provenance_note(Some("authored from the investigation"), Some("db54260f")).as_deref(),
            Some("authored from the investigation (agent write, session db54260f)")
        );
        assert_eq!(provenance_note(None, None).as_deref(), Some("agent write"));

        let payload = build_agent_upsert(&req("plan", "s"), "# T\n".to_string());
        assert_eq!(
            payload.change_description.as_deref(),
            Some("agent write, session db54260f")
        );
    }

    // ---- body resolution -------------------------------------------------

    #[test]
    fn body_wins_and_source_path_is_read_when_body_is_absent() {
        let mut r = req("plan", "s");
        assert_eq!(resolve_body(&r).unwrap(), "# T\n");

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("p.md");
        std::fs::write(&path, "# From disk\n").unwrap();
        r.body = None;
        r.source_path = Some(path.to_string_lossy().to_string());
        assert_eq!(resolve_body(&r).unwrap(), "# From disk\n");
    }

    #[test]
    fn neither_body_nor_source_path_is_an_error() {
        let mut r = req("plan", "s");
        r.body = None;
        r.source_path = None;
        let err = resolve_body(&r).unwrap_err();
        assert!(err.contains("body"), "got {err}");
        assert!(err.contains("source_path"), "got {err}");
    }

    #[test]
    fn a_missing_or_non_file_source_path_is_an_error() {
        let mut r = req("plan", "s");
        r.body = None;
        r.source_path = Some("Z:/definitely/not/here.md".to_string());
        assert!(resolve_body(&r).is_err());

        let tmp = tempfile::tempdir().unwrap();
        r.source_path = Some(tmp.path().to_string_lossy().to_string());
        let err = resolve_body(&r).unwrap_err();
        assert!(err.contains("not a file"), "got {err}");
    }

    // ---- edges -----------------------------------------------------------

    #[test]
    fn an_edge_needs_exactly_one_end() {
        let both = EdgeSpec {
            to_id: Some("a".into()),
            from_id: Some("b".into()),
            relation: "feeds".into(),
            note: None,
        };
        assert!(edge_payload(&both, "anchor").is_err());

        let neither = EdgeSpec {
            to_id: None,
            from_id: None,
            relation: "feeds".into(),
            note: None,
        };
        assert!(edge_payload(&neither, "anchor").is_err());
    }

    #[test]
    fn an_outgoing_edge_anchors_on_the_written_artifact() {
        let spec = EdgeSpec {
            to_id: Some("peer".into()),
            from_id: None,
            relation: "authored_plan".into(),
            note: Some("chain".into()),
        };
        let (anchor, body) = edge_payload(&spec, "me").unwrap();
        assert_eq!(anchor, "me");
        assert_eq!(body["to_id"], serde_json::json!("peer"));
        assert_eq!(body["relation"], serde_json::json!("authored_plan"));
        assert!(body.get("from_id").is_none());
    }

    #[test]
    fn an_incoming_edge_still_anchors_on_the_written_artifact() {
        let spec = EdgeSpec {
            to_id: None,
            from_id: Some("producer".into()),
            relation: "produced_report".into(),
            note: None,
        };
        let (anchor, body) = edge_payload(&spec, "me").unwrap();
        assert_eq!(anchor, "me");
        assert_eq!(body["from_id"], serde_json::json!("producer"));
        assert!(body.get("to_id").is_none());
    }

    // ---- query forwarding -------------------------------------------------

    /// Only the documented parameters are forwarded — an unrecognized one is
    /// dropped rather than passed through, so a typo cannot silently widen a
    /// read or reach an upstream parameter this door does not mean to expose.
    #[test]
    fn only_known_query_params_are_forwarded() {
        let mut params = HashMap::new();
        params.insert("q".to_string(), "merge train".to_string());
        params.insert("limit".to_string(), "10".to_string());
        params.insert("organization_id".to_string(), "sneaky".to_string());
        params.insert("nonsense".to_string(), "x".to_string());

        let forwarded: HashMap<String, String> = params
            .into_iter()
            .filter(|(k, _)| SEARCH_PARAMS.contains(&k.as_str()))
            .collect();
        assert_eq!(forwarded.len(), 2);
        assert_eq!(forwarded.get("q").map(String::as_str), Some("merge train"));
        assert!(!forwarded.contains_key("organization_id"));
        assert!(!forwarded.contains_key("nonsense"));
    }

    #[test]
    fn the_candidate_params_are_the_documented_three() {
        assert!(CANDIDATE_PARAMS.contains(&"include_coord"));
        assert!(!CANDIDATE_PARAMS.contains(&"organization_id"));
    }

    // ---- routes ------------------------------------------------------------

    /// All four routes are registered, and the gating split is exactly what the
    /// vet added the flag for: the two WRITES gated, the two READS not.
    /// `gated_flow.rs:226-232` makes the same distinction — "knowing a view
    /// exists is inert; opening it is the privileged act" — and Phase 6's whole
    /// value is an agent being able to ask the candidate question.
    #[test]
    fn the_write_routes_are_gated_and_the_read_routes_are_not() {
        let entries = route_entries();
        assert_eq!(entries.len(), 4, "keep in lockstep with routes()");
        for (method, path, _) in entries {
            assert!(path.starts_with("/plan-library/"), "{path}");
            assert!(matches!(*method, "GET" | "POST"));
        }
        let gated: Vec<&str> = entries
            .iter()
            .filter(|(_, _, g)| *g)
            .map(|(_, p, _)| *p)
            .collect();
        assert_eq!(
            gated,
            vec!["/plan-library/artifacts", "/plan-library/links"]
        );
        let ungated: Vec<&str> = entries
            .iter()
            .filter(|(_, _, g)| !*g)
            .map(|(_, p, _)| *p)
            .collect();
        assert_eq!(
            ungated,
            vec!["/plan-library/search", "/plan-library/candidates"]
        );
        // Every gated route is a POST and every ungated one a GET — a write
        // that ever became ungated would break this too.
        assert!(entries.iter().all(|(m, _, g)| *g == (*m == "POST")));

        let _r: Router<Arc<ApiState>> = routes();
    }

    // ---- through the real router -----------------------------------------
    //
    // These drive the actual axum `Router` over a real HTTP request/response
    // pair (`tower::ServiceExt::oneshot`), so the routing, the extractors and
    // the gate are exercised together rather than asserted about in pieces.
    //
    // Only the WRITE routes are driven here, and only with the flag OFF: that
    // path returns before any network call, so the test is hermetic. Driving a
    // read route would reach the configured qontinui-web backend, making the
    // result depend on whether one happens to be running on this machine.

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// The same four routes, typed for a stateless test server. The handlers
    /// take no `State`, so `routes()`'s `Router<Arc<ApiState>>` and this are
    /// the same route table.
    fn test_app() -> Router {
        Router::new()
            .route("/plan-library/artifacts", post(write_artifact_handler))
            .route("/plan-library/links", post(create_link_handler))
    }

    async fn post_json(uri: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = test_app().oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 256 * 1024)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    /// The vet-added requirement, end to end over HTTP: with the flag unset,
    /// BOTH write routes 403 — and they do it before touching the network, so
    /// an unauthorized local caller cannot even cause an outbound request.
    #[tokio::test]
    async fn both_write_routes_403_over_http_when_the_flag_is_unset() {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[PLAN_LIBRARY_WRITE_FLAG]);
        std::env::remove_var(PLAN_LIBRARY_WRITE_FLAG);

        let (status, body) = post_json(
            "/plan-library/artifacts",
            serde_json::json!({"kind": "plan", "slug": "s", "body": "# T\n"}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["success"], serde_json::json!(false));
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains(PLAN_LIBRARY_WRITE_FLAG));

        let (status, body) = post_json(
            "/plan-library/links",
            serde_json::json!({
                "from_id": "11111111-1111-1111-1111-111111111111",
                "to_id": "22222222-2222-2222-2222-222222222222",
                "relation": "authored_plan",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains(PLAN_LIBRARY_WRITE_FLAG));
    }

    /// The gate runs FIRST — before body/edge validation. A request that is
    /// both unauthorized and malformed must read as unauthorized, or the 400
    /// would leak that the door would otherwise have accepted the shape.
    #[tokio::test]
    async fn the_gate_precedes_validation() {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[PLAN_LIBRARY_WRITE_FLAG]);
        std::env::remove_var(PLAN_LIBRARY_WRITE_FLAG);

        // No `body`, no `source_path`, empty slug — a 400 on every count.
        let (status, _) = post_json(
            "/plan-library/artifacts",
            serde_json::json!({"kind": "", "slug": ""}),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the capability gate must answer before any request validation"
        );
    }
}
