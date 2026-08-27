//! `:9876/session-repository/*` — the agent-facing door onto qontinui-web's
//! Claude Code session repository (plan
//! `2026-08-26-claude-code-session-repository-in-qontinui-web`, Phase 4's
//! runner half).
//!
//! | route | forwards to | gated |
//! |---|---|---|
//! | `GET /session-repository` | `GET {web}/api/v1/session-repository?…` | no |
//! | `GET /session-repository/unfinished` | `GET {web}/api/v1/session-repository/unfinished?…` | no |
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
//! ## Identity: the organization is NEVER accepted from the request
//!
//! Every forward attaches the **runner-resolved device JWT**
//! ([`crate::auth::attach_device_auth`]), and the web side derives
//! `organization_id` from that principal. A caller-supplied org on an
//! unauthenticated loopback surface is a scope-escalation bug, and the surest
//! defence is to give the request nowhere to put one: the forwarded query
//! parameters are an explicit allowlist and `organization_id` is not on it.
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

use axum::extract::Query;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::mcp::types::{api_error, ApiResponse, ApiState};

type ApiResult = Result<Json<ApiResponse<Value>>, (StatusCode, Json<ApiResponse<()>>)>;

/// Per-request ceiling on an upstream call. Without one a black-holing backend
/// parks the axum handler — and the connection behind it — indefinitely; the
/// sibling plan-library door sets one for exactly this reason. These are list
/// reads with a `limit` ceiling of 200 rows and no bodies, so they are much
/// cheaper than that door's body pushes and the timeout is correspondingly
/// tighter.
const UPSTREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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
             session-repository routes must accept a device bearer \
             (app.api.deps.get_audit_actor_user, or another user-or-device dependency); a route \
             wired to `current_active_user` alone is Cognito-only and rejects it.",
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

/// GET a web session-repository path, forwarding `params` as the query string.
async fn upstream_get(path: &str, params: &HashMap<String, String>) -> ApiResult {
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
    let resp = crate::auth::attach_device_auth(upstream_client().get(&url))
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

/// `GET /session-repository` — filtered, full-text read over the archived
/// session corpus. **Ungated.**
pub async fn list_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    upstream_get("/api/v1/session-repository", &allowed(params, &LIST_PARAMS)).await
}

/// `GET /session-repository/unfinished` — the capability the operator asked for
/// by name: sessions that were never closed out. **Ungated.**
///
/// The upstream response's `unknown_count` and `coord_outstanding` are relayed
/// untouched. Both are load-bearing: an empty `items` beside a large
/// `unknown_count` means the closeout derivation has not run, which is a
/// different fact from "everything was closed out" and must not be read as one.
pub async fn unfinished_handler(Query(params): Query<HashMap<String, String>>) -> ApiResult {
    upstream_get(
        "/api/v1/session-repository/unfinished",
        &allowed(params, &UNFINISHED_PARAMS),
    )
    .await
}

/// The door's route table, as data: `(method, path, gated)`.
///
/// `Router` has no public introspection, so the table is what makes the route
/// set testable at all. Keep in lockstep with the `.route(` calls in
/// [`routes`]; the test below pins them together. The `gated` column is `false`
/// on every row and is carried anyway so this family's entries have the same
/// shape as `plan_library::route_entries` — a door that later grows a gated
/// route should not have to change the tuple type to say so.
pub fn route_entries() -> &'static [(&'static str, &'static str, bool)] {
    &[
        ("GET", "/session-repository", false),
        ("GET", "/session-repository/unfinished", false),
    ]
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // The literal `/unfinished` is registered alongside the list route;
        // axum matches literals over any future `/{id}` param route, and the
        // web side documents the same ordering constraint for the same reason.
        .route("/session-repository", get(list_handler))
        .route("/session-repository/unfinished", get(unfinished_handler))
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
