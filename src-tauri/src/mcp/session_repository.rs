//! `:9876/session-repository/*` — the agent-facing door onto qontinui-web's
//! Claude Code session repository (plan
//! `2026-08-26-claude-code-session-repository-in-qontinui-web`, Phase 4's
//! runner half).
//!
//! | route | forwards to | gated |
//! |---|---|---|
//! | `GET /session-repository` | `GET {web}/api/v1/session-repository?…` | no |
//! | `GET /session-repository/unfinished` | `GET {web}/api/v1/session-repository/unfinished?…` | no |
//! | `GET /session-repository/{id}` | `GET {web}/api/v1/session-repository/{id}?…` | no |
//! | `GET /session-repository/{id}/turns` | `GET {web}/api/v1/session-repository/{id}/turns?…` | no |
//! | `GET /session-repository/{id}/export` | `GET {web}/api/v1/session-repository/{id}/export` | no |
//!
//! ## The by-id reads are what make the list reads usable
//!
//! A list row carries an `id`, and until an agent can spend it the corpus is
//! addressable only in aggregate: `GET /unfinished` names sessions it then has
//! no way to read. The sibling plan-library door draws the line in the same
//! place — `GET /plan-library/search` is paired with
//! `GET /plan-library/artifacts/{id}` — so the three by-id forwards here are
//! that door's shape, not a new one.
//!
//! `/export` is the odd one and is deliberately NOT wrapped in this family's
//! JSON envelope: it returns the archived JSONL **byte-verbatim**, with
//! upstream's `X-Content-Sha256` / `X-Content-Sha256-Stored` /
//! `X-Content-Sha256-Match` / `X-Digest-Verifiable` headers relayed unchanged.
//! Re-encoding those bytes through a JSON string would destroy the one
//! property the archive exists to hold — that the export hashes to the digest
//! of the file the scanner read.
//!
//! ## Why an HTTP route and not an MCP tool
//!
//! This is `mcp::plan_library`'s design decision **D5**, applied to the same
//! problem one corpus later — and the plan originally got it wrong, proposing
//! "a coord MCP read tool so agent sessions reach it the same way they reach
//! the plan library". They do not reach the plan library that way.
//!
//! Agents in this fleet see exactly one MCP server: `coord-mcp`, proxied by the
//! runner as a strict allowlisted passthrough that injects no runner-local
//! tools. So "add an MCP tool" can only mean "add a **coord** tool" — which
//! would re-couple coord to a corpus it does not own and does not store.
//! `grep -rn 'plan_library' qontinui-coord/src/` returns nothing, which is what
//! confirms no coord-side tool was ever the mechanism. The briefing already
//! advertises `http://127.0.0.1:{api_port}`, so a runner HTTP route is how an
//! agent actually reaches a web-owned corpus.
//!
//! ## Reads only, and deliberately so
//!
//! The plan-library door carries two WRITE routes behind a capability flag,
//! because an agent is the only thing that knows the prompt→plan edges a
//! directory scan cannot see. Nothing equivalent is true here: the session
//! corpus's writers are the Phase 1 backfill scanner (which holds the verbatim
//! bytes and talks to qontinui-web directly, not through this loopback) and the
//! web-side archiver. An agent has nothing to contribute that those two do not
//! already hold, so this door has no write path at all — and therefore no
//! capability flag, because there is no write for one to gate.
//!
//! ## Identity: the tenant is STATED, the organization is NEVER accepted
//!
//! Every forward attaches the **runner-resolved device JWT**, and the web side
//! derives `organization_id` from that principal. A caller-supplied org on an
//! unauthenticated loopback surface is a scope-escalation bug, and the surest
//! defence is to give the request nowhere to put one: the forwarded query
//! parameters are an explicit allowlist and `organization_id` is not on it.
//!
//! Which tenant's credential is presented is [`bearer_scope`]'s answer, and it
//! comes from the caller's own `tenant_id` — not from whichever binding happens
//! to be this device's default. That distinction only bites on a multi-bound
//! device, and there it is the difference between reading the tenant an agent
//! asked for and reading a different one's sessions; see that function.
//!
//! ## The two honesty signals are forwarded verbatim
//!
//! `tenant_source` and `body_source` are filterable upstream on purpose (plan
//! §3.6 rule 2 and §5), so both are on the allowlist. An agent asking "which
//! sessions were never closed out?" must be able to tell a *guessed* tenant
//! from a declared one, and a `coord_redacted` body — whose digest can never be
//! verified against the original transcript — from a `disk_verbatim` one. This
//! door narrows nothing and re-labels nothing; upstream's answer, including its
//! `unknown_count` and its `coord_outstanding` UNKNOWN signal, passes through
//! unchanged.

use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::header::{HeaderName, HeaderValue};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::auth::TenantScope;
use crate::mcp::types::{api_error, ApiResponse, ApiState};

/// The error half every route on this door returns, JSON body and all.
type ApiFailure = (StatusCode, Json<ApiResponse<()>>);
type ApiResult = Result<Json<ApiResponse<Value>>, ApiFailure>;

/// Per-request ceiling on an upstream call. Without one a black-holing backend
/// parks the axum handler — and the connection behind it — indefinitely; the
/// sibling plan-library door sets one for exactly this reason. These are list
/// reads with a `limit` ceiling of 200 rows and no bodies, so they are much
/// cheaper than that door's body pushes and the timeout is correspondingly
/// tighter.
const UPSTREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// The same ceiling for `/export`, which is a different cost class: the
/// response is a whole archived transcript (the plan measures a p99 of 4 MB),
/// not a bounded page of head rows. Sized against that body rather than
/// against the list reads, and still finite for the reason
/// [`UPSTREAM_TIMEOUT`] exists at all.
const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Refuse to buffer an export larger than this rather than growing the
/// runner's heap by whatever the backend decides to send. Ten times the plan's
/// measured p99, so a real transcript is never the thing that trips it.
const EXPORT_MAX_BYTES: usize = 40 * 1024 * 1024;

/// The one long-lived HTTP client every upstream call shares.
///
/// `reqwest::Client` owns the connection pool and the TLS configuration, so
/// building one per request rebuilds both every time and makes connection reuse
/// impossible. Both siblings already hold a single client for the process
/// (`mcp::plan_library::upstream_client`,
/// `plan_workunit_adapter::body_push::HttpArtifactSink`).
fn upstream_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(UPSTREAM_TIMEOUT)
            .build()
            // `build()` only fails on a broken TLS backend, which the default
            // client would hit too. Degrade rather than panic in a handler.
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "session-repository door: could not build a timeout-bearing HTTP client; \
                     falling back to the default (no timeout)"
                );
                reqwest::Client::new()
            })
    })
}

/// The client `/export` uses, separate only because it carries
/// [`EXPORT_TIMEOUT`] instead of [`UPSTREAM_TIMEOUT`]. Still one per process,
/// for the connection-pool reason above.
fn export_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(EXPORT_TIMEOUT)
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    "session-repository door: could not build a timeout-bearing export client;                      falling back to the default (no timeout)"
                );
                reqwest::Client::new()
            })
    })
}

/// Which tenant's device credential to present on a forward.
///
/// The caller's own `tenant_id` decides it. That parameter is on the list
/// route's allowlist as a FILTER, and it is read here for a second, separate
/// purpose — an agent asking for tenant X's sessions must be answered with
/// X's bearer, because qontinui-web derives `organization_id` from the
/// verified principal and nothing else. On the by-id routes upstream accepts
/// no `tenant_id` at all, so there it is credential-only: read here, dropped
/// by the allowlist, never forwarded.
///
/// A caller that names none is [`TenantScope::Unresolved`], never the default
/// binding. On a single-bound device that resolves to exactly the credential
/// this door presented before, so nothing changes; on a multi-bound one it
/// degrades to unauthenticated rather than silently answering out of whichever
/// tenant happens to be default — which is a cross-tenant READ, not merely a
/// mis-attributed one, and is the whole reason the scope is stated rather than
/// defaulted.
fn bearer_scope(params: &HashMap<String, String>) -> TenantScope {
    TenantScope::for_session(
        params
            .get("tenant_id")
            .and_then(|t| uuid::Uuid::parse_str(t.trim()).ok()),
    )
}

/// The qontinui-web base URL, trailing slash trimmed.
fn web_base() -> String {
    crate::api_config::get_api_base_url()
        .trim_end_matches('/')
        .to_string()
}

/// Turn a non-2xx upstream answer into this door's error, **verbatim**.
///
/// The upstream status code and body pass through unchanged rather than being
/// remapped to a generic 500, so an agent sees the real 422 field error and the
/// real 404. The one thing added is the 401/403 diagnosis, because a bare 401
/// here is systematically misleading — it is far more likely to be the
/// dependency mismatch below than a genuinely unauthorized caller.
fn upstream_failure(what: &str, status: reqwest::StatusCode, text: String) -> ApiFailure {
    let code = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut message = format!("{what}: upstream {status}: {text}");
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        message.push_str(
            "\nnote: this door presents the runner's coord-issued DEVICE JWT. The qontinui-web \
             session-repository routes must accept a device bearer \
             (app.api.deps.get_audit_actor_user, or another user-or-device dependency); a route \
             wired to `current_active_user` alone is Cognito-only and rejects it.\nnote: on a \
             device paired to MORE THAN ONE tenant, a request naming no `tenant_id` is \
             deliberately sent UNAUTHENTICATED rather than read under whichever binding is \
             default — pass `tenant_id=<uuid>` to say which tenant to read as.",
        );
    }
    (code, Json(api_error(message)))
}

/// Transport failure (DNS, refused connection, timeout) — the backend is not
/// answering at all, which is a different thing from it answering "no".
fn transport_failure(what: &str, e: reqwest::Error) -> ApiFailure {
    (
        StatusCode::BAD_GATEWAY,
        Json(api_error(format!(
            "{what}: could not reach the qontinui-web backend at {}: {e}",
            web_base()
        ))),
    )
}

/// Build the upstream URL for `path` with `params` as its query string.
fn upstream_url(path: &str, params: &HashMap<String, String>) -> String {
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
    url
}

/// GET a web session-repository path, forwarding `params` as the query string
/// and presenting `scope`'s credential.
///
/// `scope` is [`bearer_scope`]'s answer over the caller's RAW parameters, not
/// over the allowlisted ones: on the by-id routes `tenant_id` selects the
/// credential without being forwarded.
async fn upstream_get(
    path: &str,
    params: &HashMap<String, String>,
    scope: TenantScope,
) -> ApiResult {
    let url = upstream_url(path, params);
    let resp = crate::auth::attach_device_auth_for(upstream_client().get(&url), scope)
        .send()
        .await
        .map_err(|e| transport_failure(path, e))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(upstream_failure(path, status, text));
    }
    let value: Value = serde_json::from_str(&text).map_err(|e| {
        (
            StatusCode::BAD_GATEWAY,
            Json(api_error(format!("{path}: unparseable upstream body: {e}"))),
        )
    })?;
    Ok(Json(ApiResponse::success(value)))
}

/// Query parameters this door forwards to the web list route.
///
/// An explicit allowlist, not a passthrough: anything else is DROPPED rather
/// than relayed, so neither a typo nor a caller-supplied `organization_id` can
/// silently widen a read. `tenant_source` and `body_source` are on it because
/// the plan makes both filterable on purpose — see the module doc.
const LIST_PARAMS: [&str; 16] = [
    "account",
    "repo",
    "state",
    "closeout_state",
    "tenant_id",
    "tenant_source",
    "body_source",
    "machine_id",
    "work_unit_slug",
    "has_secret_findings",
    "secret_finding_kind",
    "detector_ran",
    "since",
    "q",
    "offset",
    "limit",
];

/// Query parameters forwarded to the web `unfinished` route — the smaller set
/// that route actually accepts.
const UNFINISHED_PARAMS: [&str; 5] = ["account", "repo", "since", "offset", "limit"];

fn allowed(params: HashMap<String, String>, allow: &[&str]) -> HashMap<String, String> {
    params
        .into_iter()
        .filter(|(k, _)| allow.contains(&k.as_str()))
        .collect()
}

/// Parameters the two by-id JSON reads forward.
///
/// `include_turn_index` / `turn_index_limit` belong to the detail route and
/// `from` / `limit` / `include_raw` to `/turns`; one allowlist covers both
/// because a parameter upstream does not know is ignored there, and a second
/// near-identical constant is the kind of duplication that drifts. `tenant_id`
/// is deliberately absent: it selects the credential ([`bearer_scope`]), and
/// upstream has no tenant filter on these routes — forwarding it would put a
/// filter on the wire that nothing applies.
const BY_ID_PARAMS: [&str; 5] = [
    "include_turn_index",
    "turn_index_limit",
    "from",
    "limit",
    "include_raw",
];

/// The upstream path for one archived session, or the 400 text for an `id`
/// that is not a UUID.
///
/// Refused HERE, before a dial, for the reason the sibling plan-library door
/// gives: a 400 naming the shape beats a 422 from upstream after a round trip
/// — and an id that never reaches the URL cannot smuggle a path segment into
/// it either.
fn session_upstream_path(raw: &str, suffix: &str) -> Result<String, String> {
    match uuid::Uuid::parse_str(raw.trim()) {
        Ok(id) => Ok(format!(
            "/api/v1/session-repository/{}{suffix}",
            id.hyphenated()
        )),
        Err(e) => Err(format!(
            "/session-repository/{{id}}{suffix}: `id` must be a session-artifact UUID (the \
             `id` on a row from GET /session-repository), got {raw:?}: {e}"
        )),
    }
}

/// An error this door decided on its own, rather than one relayed from
/// upstream by [`upstream_failure`].
fn door_error(code: StatusCode, message: String) -> ApiFailure {
    (code, Json(api_error(message)))
}

/// `GET /session-repository` — filtered, full-text read over the archived
/// session corpus. **Ungated.**
pub async fn list_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    let scope = bearer_scope(&params);
    upstream_get(
        "/api/v1/session-repository",
        &allowed(params, &LIST_PARAMS),
        scope,
    )
    .await
}

/// `GET /session-repository/unfinished` — the capability the operator asked
/// for by name: sessions that were never closed out. **Ungated.**
///
/// The upstream response's `unknown_count` and `coord_outstanding` are relayed
/// untouched. Both are load-bearing: an empty `items` beside a large
/// `unknown_count` means the closeout derivation has not run, which is a
/// different fact from "everything was closed out" and must not be read as one.
pub async fn unfinished_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    let scope = bearer_scope(&params);
    upstream_get(
        "/api/v1/session-repository/unfinished",
        &allowed(params, &UNFINISHED_PARAMS),
        scope,
    )
    .await
}

/// `GET /session-repository/{id}` — one session's head row plus a bounded
/// index of its turns. **Ungated.**
///
/// Upstream's `turn_index_state` is relayed untouched, and it is the field
/// that matters: `not_requested` and `unavailable` are both distinct from an
/// empty index, exactly as `unknown_count` is distinct from zero on
/// `/unfinished`.
pub async fn detail_handler(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let scope = bearer_scope(&params);
    let path =
        session_upstream_path(&id, "").map_err(|m| door_error(StatusCode::BAD_REQUEST, m))?;
    upstream_get(&path, &allowed(params, &BY_ID_PARAMS), scope).await
}

/// `GET /session-repository/{id}/turns` — a page of decoded turns.
/// **Ungated.**
///
/// The route exists so an agent can read a transcript without swallowing it: a
/// 4 MB body is upstream's measured p99, and paging is how this door stays the
/// cheap read its timeout is sized for.
pub async fn turns_handler(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> ApiResult {
    let scope = bearer_scope(&params);
    let path =
        session_upstream_path(&id, "/turns").map_err(|m| door_error(StatusCode::BAD_REQUEST, m))?;
    upstream_get(&path, &allowed(params, &BY_ID_PARAMS), scope).await
}

/// Response headers relayed verbatim from an `/export` forward.
///
/// The four digest headers are the point of the route: `X-Content-Sha256` is
/// upstream's digest of the bytes it actually sent, `X-Content-Sha256-Stored`
/// the one recorded on the row, `X-Content-Sha256-Match` whether they agree,
/// and `X-Digest-Verifiable` whether the digest can be compared with the
/// ORIGINAL transcript at all. Dropping any of them would leave a caller
/// holding bytes it cannot verify while looking as though it could.
const EXPORT_RELAY_HEADERS: [&str; 9] = [
    "content-type",
    "content-disposition",
    "x-content-sha256",
    "x-content-sha256-stored",
    "x-content-sha256-match",
    "x-digest-verifiable",
    "x-body-source",
    "x-claude-session-id",
    // One of the two honesty signals this door promises to forward verbatim
    // (the other, `body_source`, rides on `X-Body-Source` above).
    "x-tenant-source",
];

/// The refusal for an export bigger than [`EXPORT_MAX_BYTES`], naming the
/// paged route that can read it instead.
fn export_too_large(path: &str, len: u64) -> ApiFailure {
    (
        StatusCode::BAD_GATEWAY,
        Json(api_error(format!(
            "{path}: upstream body is {len} bytes, over this door's {EXPORT_MAX_BYTES}-byte \
             export ceiling. The archive is unaffected — read the transcript in pages through \
             GET /session-repository/{{id}}/turns instead."
        ))),
    )
}

/// `GET /session-repository/{id}/export` — the archived JSONL, byte-verbatim.
/// **Ungated.**
///
/// The one route on this door that does NOT wrap its answer in
/// [`ApiResponse`]. The archive's whole claim is that an export hashes to the
/// digest of the file the scanner read; re-encoding those bytes as a JSON
/// string would break that for any transcript that is not valid UTF-8 and
/// would leave the digest headers describing something other than what was
/// sent. So the body passes through untouched and so do the headers that
/// describe it.
pub async fn export_handler(
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiFailure> {
    let scope = bearer_scope(&params);
    let path = session_upstream_path(&id, "/export")
        .map_err(|m| door_error(StatusCode::BAD_REQUEST, m))?;
    let url = upstream_url(&path, &HashMap::new());
    let resp = crate::auth::attach_device_auth_for(export_client().get(&url), scope)
        .send()
        .await
        .map_err(|e| transport_failure(&path, e))?;
    let status = resp.status();
    let headers = resp.headers().clone();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(upstream_failure(&path, status, text));
    }
    // Checked BEFORE buffering where upstream declares a length, and again
    // after where it does not: refusing an oversize body only once it is
    // already on the heap would defeat the point of having a ceiling.
    if let Some(len) = resp.content_length() {
        if len > EXPORT_MAX_BYTES as u64 {
            return Err(export_too_large(&path, len));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| transport_failure(&path, e))?;
    if bytes.len() > EXPORT_MAX_BYTES {
        return Err(export_too_large(&path, bytes.len() as u64));
    }

    let mut out = Response::builder().status(StatusCode::OK);
    for name in EXPORT_RELAY_HEADERS {
        if let Some(value) = headers.get(name) {
            if let (Ok(n), Ok(v)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                out = out.header(n, v);
            }
        }
    }
    out.body(Body::from(bytes)).map_err(|e| {
        door_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{path}: could not build the export response: {e}"),
        )
    })
}

/// The door's route table, as data: `(method, path, gated)`.
///
/// `Router` has no public introspection, so the table is what makes the route
/// set testable at all. Keep in lockstep with the `.route(` calls in
/// [`routes`]; the test below pins them together. The `gated` column is `false`
/// on every row and is carried anyway so this family's entries have the same
/// shape as `plan_library::route_entries` -- a door that later grows a gated
/// route should not have to change the tuple type to say so.
pub fn route_entries() -> &'static [(&'static str, &'static str, bool)] {
    &[
        ("GET", "/session-repository", false),
        ("GET", "/session-repository/unfinished", false),
        ("GET", "/session-repository/{id}", false),
        ("GET", "/session-repository/{id}/turns", false),
        ("GET", "/session-repository/{id}/export", false),
    ]
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // The literal `/unfinished` is registered before the `/{id}` param
        // route; axum matches literals over params regardless of order, and the
        // web side documents the same ordering constraint for the same reason.
        .route("/session-repository", get(list_handler))
        .route("/session-repository/unfinished", get(unfinished_handler))
        .route("/session-repository/{id}", get(detail_handler))
        .route("/session-repository/{id}/turns", get(turns_handler))
        .route("/session-repository/{id}/export", get(export_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drift guard: every `.route(` call in [`routes`] must have a
    /// [`route_entries`] row, and vice versa. Same shape as the plan-library
    /// door's own guard — `mcp::ui_bridge::mod::manifest_matches_route_calls`
    /// deliberately scans only `/ui-bridge/*` literals, so a family mounted at
    /// the router root is invisible to it and needs its own.
    #[test]
    fn route_entries_match_the_registered_routes() {
        let source = include_str!("session_repository.rs");
        let registered: Vec<String> = source
            .lines()
            .filter_map(|l| l.trim().strip_prefix(".route(\""))
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect();
        let declared: Vec<String> = route_entries()
            .iter()
            .map(|(_, path, _)| (*path).to_string())
            .collect();
        assert_eq!(
            registered, declared,
            "routes() and route_entries() drifted apart"
        );
    }

    #[test]
    fn the_allowlist_drops_anything_it_does_not_name() {
        let mut params = HashMap::new();
        params.insert("tenant_source".to_string(), "ambiguous".to_string());
        params.insert("q".to_string(), "merge train".to_string());
        // The two that must never reach upstream from a loopback caller.
        params.insert("organization_id".to_string(), "attacker".to_string());
        params.insert("tenant_sauce".to_string(), "typo".to_string());

        let kept = allowed(params, &LIST_PARAMS);
        assert_eq!(kept.len(), 2);
        assert_eq!(
            kept.get("tenant_source").map(String::as_str),
            Some("ambiguous")
        );
        assert!(!kept.contains_key("organization_id"));
        assert!(!kept.contains_key("tenant_sauce"));
    }

    #[test]
    fn the_unfinished_allowlist_is_the_narrower_one_upstream_accepts() {
        let mut params = HashMap::new();
        params.insert("account".to_string(), "gmail".to_string());
        // Accepted by the LIST route, not by `unfinished` — forwarding it
        // would earn a 422 that reads like the agent's query was wrong.
        params.insert("tenant_source".to_string(), "unknown".to_string());
        let kept = allowed(params, &UNFINISHED_PARAMS);
        assert_eq!(kept.len(), 1);
        assert!(kept.contains_key("account"));
    }

    #[test]
    fn the_by_id_allowlist_drops_the_credential_only_tenant_id() {
        let mut params = HashMap::new();
        params.insert("include_turn_index".to_string(), "false".to_string());
        params.insert("from".to_string(), "40".to_string());
        // Read by `bearer_scope` to pick the credential, and upstream has no
        // such parameter on the by-id routes -- forwarding it would earn a 422
        // that reads like the agent's query was wrong.
        params.insert(
            "tenant_id".to_string(),
            "6b2a4a6e-0000-0000-0000-000000000001".to_string(),
        );

        let kept = allowed(params, &BY_ID_PARAMS);
        assert_eq!(kept.len(), 2);
        assert!(!kept.contains_key("tenant_id"));
    }

    #[test]
    fn the_bearer_scope_comes_from_the_callers_tenant_and_never_defaults() {
        let tenant = uuid::Uuid::parse_str("6b2a4a6e-0000-0000-0000-000000000001").unwrap();
        let mut named = HashMap::new();
        named.insert("tenant_id".to_string(), format!("  {tenant}  "));
        assert_eq!(bearer_scope(&named), TenantScope::Owned(tenant));

        // Not `Device`. `Device` would assert that this route takes no tenancy
        // from the bearer, which is false -- qontinui-web resolves the org from
        // the principal, so the credential decides WHICH tenant's sessions are
        // listed. `Unresolved` is what makes the multi-bound degrade fire.
        assert_eq!(bearer_scope(&HashMap::new()), TenantScope::Unresolved);

        let mut junk = HashMap::new();
        junk.insert("tenant_id".to_string(), "not-a-uuid".to_string());
        assert_eq!(bearer_scope(&junk), TenantScope::Unresolved);
    }

    #[test]
    fn a_by_id_path_is_refused_before_a_dial_unless_it_is_a_uuid() {
        let id = "6B2A4A6E-0000-0000-0000-000000000001";
        assert_eq!(
            session_upstream_path(id, "/turns").unwrap(),
            "/api/v1/session-repository/6b2a4a6e-0000-0000-0000-000000000001/turns"
        );
        // A traversal attempt never reaches the URL: it is not a UUID.
        let refused = session_upstream_path("../../admin", "").unwrap_err();
        assert!(
            refused.contains("must be a session-artifact UUID"),
            "{refused}"
        );
    }

    #[test]
    fn the_door_carries_no_write_route() {
        // Not a style assertion: the corpus's writers hold bytes and
        // credentials this loopback surface does not, so a write here would be
        // an unauthenticated local process using the runner's own credential to
        // put content into the operator's session archive.
        let source = include_str!("session_repository.rs");
        // The needle is assembled from two literals so this assertion does not
        // match its own source line.
        let post_import = concat!("axum::routing::", "post");
        assert!(
            !source.contains(post_import),
            "a write route appeared on a read-only door"
        );
        assert!(route_entries()
            .iter()
            .all(|(method, _, _)| *method == "GET"));
    }
}
