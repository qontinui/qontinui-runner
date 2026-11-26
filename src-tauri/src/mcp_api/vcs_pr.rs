//! Loopback PR-creation proxy (plan qontinui-pr-credential-provisioning, 2b):
//! `POST /vcs/pull-requests` and its nonce-gated device-JWT forwarder. Moved
//! verbatim out of `mcp_api.rs` (plan
//! `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`
//! Phase 2).

use axum::response::Json;
use tracing::warn;

use super::github_segment_is_valid;

/// Request body for `POST /vcs/pull-requests`. `repo` is `owner/name`; every
/// other field is forwarded to coord's PR-creation route (the coord body is
/// this struct MINUS `repo`, which travels in the coord URL path instead).
#[derive(Debug, Clone, serde::Deserialize)]
struct VcsPullRequestBody {
    repo: String,
    head: String,
    #[serde(default)]
    base: Option<String>,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: Option<bool>,
}

/// Split + validate an `owner/name` repo slug. Each segment must start with an
/// ASCII alphanumeric and continue in `[A-Za-z0-9._-]`, with `.`/`..` rejected —
/// so a segment can never smuggle a path separator or escape sequence into the
/// fixed coord route template (same boundary as [`super::slug_is_valid`] /
/// [`super::gate_id_is_valid`] on the coord-mcp write forwarder).
fn parse_owner_repo(repo: &str) -> Option<(&str, &str)> {
    let (owner, name) = repo.split_once('/')?;
    let segment_ok = github_segment_is_valid;
    if segment_ok(owner) && segment_ok(name) && !name.contains('/') {
        Some((owner, name))
    } else {
        None
    }
}

/// Build coord's PR-creation URL for an (already-validated) `owner`/`name`:
/// `POST {coord}/coord/repos/{owner}/{repo}/pull-requests`.
fn vcs_pr_upstream_url(base: &str, owner: &str, name: &str) -> String {
    format!(
        "{}/coord/repos/{owner}/{name}/pull-requests",
        base.trim_end_matches('/')
    )
}

/// The JSON body forwarded to coord: the inbound body MINUS `repo` (which is
/// path-encoded), with absent optional fields omitted rather than nulled.
fn vcs_pr_upstream_body(req: &VcsPullRequestBody) -> serde_json::Value {
    let mut body = serde_json::json!({
        "head": req.head,
        "title": req.title,
    });
    let obj = body.as_object_mut().expect("literal object");
    if let Some(base) = &req.base {
        obj.insert("base".to_string(), serde_json::json!(base));
    }
    if let Some(b) = &req.body {
        obj.insert("body".to_string(), serde_json::json!(b));
    }
    if let Some(draft) = req.draft {
        obj.insert("draft".to_string(), serde_json::json!(draft));
    }
    body
}

/// `POST /vcs/pull-requests` — the nonce-gated PR-creation forwarder (plan
/// qontinui-pr-credential-provisioning, Phase 2b).
///
/// Why: agents in runner-hosted terminals have no personal GitHub login on the
/// machine, so `gh pr create` fails. Coord brokers PR creation with its own
/// installation credential (`POST /coord/repos/{owner}/{repo}/pull-requests`,
/// JWT-authed — coord accepts BOTH device and agent bearers on that route);
/// this loopback route lets any session that holds a per-session coord-mcp
/// proxy nonce reach it — the `qontinui-pr create` CLI is the intended caller.
///
/// Gate: exactly the coord-mcp proxy discipline — a registered proxy nonce
/// (from `Authorization: Bearer <nonce>` or the legacy
/// `X-Coord-Mcp-Proxy-Key`), Device- OR Agent-bound (coord-spawned agent
/// sessions get Agent-bound nonces via `write_coord_mcp_agent_proxy_config`
/// and are this feature's primary population), and a live bearer matched to
/// the nonce's principal by [`crate::coord_mcp::proxy_request_gate`] (device
/// JWT for Device nonces, THAT agent's own refreshed JWT for Agent nonces —
/// the nonce→principal binding prevents scope elevation in either direction).
/// A device JWT that stays missing after the bounded
/// [`crate::coord_mcp::DEVICE_JWT_REMINT_WAIT`] re-mint wait is a 503
/// `runner not paired — no device JWT` (a pairing gap, not an auth failure).
/// Coord's status + body pass through verbatim so 403/404 (repo not in the
/// caller tenant) and 429 (rate limit) surface honestly; runner-originated
/// failures use distinct `VCS_PR_PROXY_*` codes.
pub(super) async fn vcs_create_pull_request_handler(
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Phase 2 (plan 2026-08-20): resolved through THE shared request-side
    // resolver, which prefers `Authorization: Bearer <nonce>` and keeps the
    // legacy `X-Coord-Mcp-Proxy-Key` accepted. Carrying the nonce in
    // `Authorization` is what stops the MCP client attaching an OAuth provider
    // to this server, and therefore what stops a stale key's 401 escalating
    // into OAuth discovery -> Dynamic Client Registration -> this runner's own
    // 404 -> a durable `mcpOAuth` poison entry.
    let nonce = crate::coord_mcp::proxy_nonce_from_request(&headers);

    // Resolve the principal FROM THE NONCE first — the binding decides which
    // identity's bearer this route injects (device JWT vs the agent's own
    // JWT). An absent/unregistered nonce 401s before any body parsing or
    // credential I/O.
    let principal = match nonce
        .as_deref()
        .and_then(crate::coord_mcp::proxy_principal_for_nonce)
    {
        Some(p) => p,
        None => {
            warn!("vcs pr proxy: {}", crate::coord_mcp::STALE_PROXY_KEY_CAUSE);
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "success": false,
                    "error": crate::coord_mcp::stale_proxy_key_error(
                        crate::coord_mcp::STALE_PROXY_KEY_CAUSE,
                    ),
                    "code": "VCS_PR_PROXY_UNAUTHORIZED",
                })),
            )
                .into_response();
        }
    };

    // Parse + validate the request BEFORE any credential I/O — a bad shape is
    // a runner-originated 400, never forwarded.
    let req: VcsPullRequestBody = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            // A body that is not UTF-8 is an ENCODING problem, not a shape
            // one — the same diagnosis the `Json<T>` extractor paths and the
            // plan-library write door give (`envelope::invalid_utf8_message`).
            // PR bodies are full of em dashes, which Git Bash curl sends as
            // the single ANSI byte `0x97`.
            if let Some(msg) = crate::mcp::envelope::invalid_utf8_message(&e.to_string()) {
                return crate::mcp::envelope::envelope_400_invalid_utf8(msg).into_response();
            }
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("invalid request body: {e}"),
                    "code": "VCS_PR_PROXY_BAD_REQUEST",
                })),
            )
                .into_response();
        }
    };
    let (owner, name) = match parse_owner_repo(&req.repo) {
        Some(pair) => pair,
        None => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!(
                        "invalid repo (expected owner/name): {:?}",
                        req.repo
                    ),
                    "code": "VCS_PR_PROXY_BAD_REQUEST",
                })),
            )
                .into_response();
        }
    };
    if req.head.trim().is_empty() || req.title.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": "head and title are required",
                "code": "VCS_PR_PROXY_BAD_REQUEST",
            })),
        )
            .into_response();
    }

    // Pick the bearer by principal — the same per-principal token discipline
    // as `coord_mcp_proxy_handler`.
    let bearer = match &principal {
        crate::coord_mcp::ProxyPrincipal::Device => {
            // B3: select the bearer for the SESSION's tenant (frozen on the
            // nonce), NOT the legacy default slot — a non-default miss returns
            // `None` (the degrade path below), never another tenant's token.
            // An unresolvable-by-any-route machine is refused (Phase 3).
            // Phase 3 (memory-injection plan): pin resolution + the
            // fail-closed refusal live in ONE helper shared by all four
            // bearer-selection sites, so the policy cannot drift between them.
            let (session_tenant, initial_tok) =
                match crate::coord_mcp::session_bearer_and_tenant_or_refuse(nonce.clone()).await {
                    Ok(pair) => pair,
                    Err(refusal) => {
                        warn!("vcs pr proxy: {}", refusal.message);
                        return (
                            axum::http::StatusCode::from_u16(refusal.status)
                                .unwrap_or(axum::http::StatusCode::SERVICE_UNAVAILABLE),
                            Json(refusal.json_body()),
                        )
                            .into_response();
                    }
                };
            // The live device JWT was read alongside the pin decision above.
            let mut tok = initial_tok;
            // A momentarily-missing device JWT is usually the refresher's
            // re-mint window, not a dead pairing — kick the refresher and wait
            // the bounded DEVICE_JWT_REMINT_WAIT (~5s) before degrading, the
            // same idiom as the coord-mcp proxy. Only a JWT that is STILL
            // missing after the wait reports the hard 503.
            if tok.as_deref().map(str::trim).unwrap_or("").is_empty() {
                tok = crate::coord_mcp::await_device_jwt_remint_for(session_tenant).await;
            }
            match tok {
                Some(t) if !t.trim().is_empty() => Some(t),
                _ => {
                    warn!(
                        "vcs pr proxy: no device JWT after bounded re-mint wait — \
                         runner not paired"
                    );
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "success": false,
                            "error": "runner not paired — no device JWT",
                            "code": "VCS_PR_PROXY_NOT_PAIRED",
                        })),
                    )
                        .into_response();
                }
            }
        }
        crate::coord_mcp::ProxyPrincipal::Agent { agent_id } => {
            match crate::coord_mcp::lookup_agent_token(*agent_id) {
                Some(slot) => {
                    let (coord_base, _coord_base_source) =
                        crate::coord_mcp::coord_base_url_with_source();
                    let _ = crate::agent_token::maybe_refresh(
                        &slot,
                        &coord_base,
                        *agent_id,
                        "vcs_pr_proxy",
                    )
                    .await;
                    Some(slot.read().await.token.clone())
                }
                None => {
                    warn!(
                        "vcs pr proxy: no live token slot for agent_id={agent_id} \
                         (torn-down or restarted agent) — failing closed"
                    );
                    return (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(serde_json::json!({
                            "success": false,
                            "error": crate::coord_mcp::stale_proxy_key_error(
                                crate::coord_mcp::AGENT_GONE_PROXY_CAUSE,
                            ),
                            "code": "VCS_PR_PROXY_AGENT_GONE",
                        })),
                    )
                        .into_response();
                }
            }
        }
    };

    if let Err((status, msg)) =
        crate::coord_mcp::proxy_request_gate(nonce.as_deref(), bearer.as_deref(), &principal)
    {
        warn!("vcs pr proxy: {msg}");
        return (
            axum::http::StatusCode::from_u16(status)
                .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
            Json(serde_json::json!({
                "success": false,
                "error": msg,
                "code": "VCS_PR_PROXY_UNAUTHORIZED",
            })),
        )
            .into_response();
    }
    let bearer = bearer.unwrap_or_default(); // gate guarantees Some(non-empty)

    // Same coord-base resolution as every other loopback forwarder
    // (`COORD_HTTP_URL` override → profiles resolver → dev localhost).
    let (coord_base, coord_base_source) = crate::coord_mcp::coord_base_url_with_source();
    let url = vcs_pr_upstream_url(&coord_base, owner, name);
    let upstream_body = vcs_pr_upstream_body(&req);
    // The source is threaded through now rather than discarded at the `_`: this
    // was the ONE forwarder whose 502 named neither the URL it dialled nor
    // where that base came from, which is exactly what a misconfigured-upstream
    // 502 has to say to be actionable.
    forward_vcs_pr_post(&url, &bearer, &upstream_body, coord_base_source).await
}

/// Forward the PR-creation POST to coord and return coord's status + headers +
/// body verbatim. Split from the handler (URL + bearer + body as plain params,
/// no `AuthManager`/env reads) so the forwarding leg is unit-testable against a
/// local mock coord with a synthetic bearer — same seam discipline as
/// [`super::forward_coord_write_post`], but with the plan's 10s overall timeout (PR
/// creation is a single bounded GitHub write behind coord).
async fn forward_vcs_pr_post(
    url: &str,
    bearer: &str,
    body: &serde_json::Value,
    coord_base_source: qontinui_runner_lib::profiles::CoordBaseSource,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    use crate::util::egress_context as egress;
    const EG: crate::util::egress_context::EgressClient =
        crate::util::egress_context::EgressClient::VcsPr;

    static VCS_PR_PROXY_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    let client = VCS_PR_PROXY_CLIENT.get_or_init(|| {
        qontinui_runner_lib::outbound_net::client_builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("vcs pr proxy reqwest client")
    });

    let upstream = {
        let _in_flight = egress::in_flight(EG);
        // coord-auth-exempt(forwarder): VCS-PR forwarder hop — same posture as
        // `forward_coord_write_post`; the caller owns the bearer.
        client.post(url).bearer_auth(bearer).json(body).send().await
    };
    let upstream = match upstream {
        Ok(resp) => resp,
        Err(e) => {
            let chain = crate::util::error_chain::error_chain(&e);
            egress::record_failure(EG);
            let ctx = egress::snapshot(EG);
            warn!(
                "vcs pr proxy: forward to {url} failed \
                 (coord_base_source={coord_base_source}): {chain} egress={ctx}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord PR endpoint unreachable: {chain}"),
                    "code": "VCS_PR_PROXY_UPSTREAM_UNREACHABLE",
                    // Parity with the other three forwarders. This door alone
                    // carried NEITHER field, so a misconfigured coord base
                    // produced a 502 that could not say which base it dialled
                    // or where that base came from.
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                    "egress": ctx,
                })),
            )
                .into_response();
        }
    };

    let status = upstream.status().as_u16();
    let mut builder = axum::http::Response::builder()
        .status(axum::http::StatusCode::from_u16(status).unwrap_or(axum::http::StatusCode::OK));
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_bytes());
    }
    let bytes = match upstream.bytes().await {
        Ok(b) => b,
        Err(e) => {
            let chain = crate::util::error_chain::error_chain(&e);
            egress::record_failure(EG);
            let ctx = egress::snapshot(EG);
            warn!(
                "vcs pr proxy: reading coord response body from {url} failed \
                 (coord_base_source={coord_base_source}): {chain} egress={ctx}"
            );
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord PR response read failed: {chain}"),
                    "code": "VCS_PR_PROXY_UPSTREAM_READ_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                    "egress": ctx,
                })),
            )
                .into_response();
        }
    };
    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|e| {
            // This arm used to return a BARE 502 with no body at all — the one
            // failure on this door a caller could learn nothing whatsoever
            // from. It now answers in the same envelope shape as its three
            // siblings.
            let chain = crate::util::error_chain::error_chain(&e);
            warn!(
                "vcs pr proxy: response build failed \
                 (upstream_url={url}, coord_base_source={coord_base_source}): {chain}"
            );
            (
                axum::http::StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "success": false,
                    "error": format!("coord PR response build failed: {chain}"),
                    "code": "VCS_PR_PROXY_RESPONSE_BUILD_FAILED",
                    "upstream_url": url,
                    "coord_base_source": coord_base_source.as_str(),
                    "egress": egress::snapshot(EG),
                })),
            )
                .into_response()
        })
}

/// Nonce-gated device-JWT PR-creation forwarder (plan
/// qontinui-pr-credential-provisioning, Phase 2b).
///
/// Same shape as `coord_write_proxy_tests`: the gate's 401 paths are asserted
/// through the real route handler (missing/wrong nonce — never forwarded); the
/// request-shaping helpers (`parse_owner_repo`, `vcs_pr_upstream_url`,
/// `vcs_pr_upstream_body`) are pure functions tested directly; the forwarding
/// leg is tested through the `forward_vcs_pr_post` seam against a local mock
/// coord with a synthetic bearer.
#[cfg(test)]
mod vcs_pr_proxy_tests {
    use super::{
        forward_vcs_pr_post, parse_owner_repo, vcs_create_pull_request_handler,
        vcs_pr_upstream_body, vcs_pr_upstream_url, VcsPullRequestBody,
    };
    use axum::{body::Body, http::Request, routing::post, Router};
    use tower::ServiceExt;

    fn vcs_router() -> Router {
        Router::new().route("/vcs/pull-requests", post(vcs_create_pull_request_handler))
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The owner/name validator: accepts real GitHub slugs, rejects
    /// path-smuggling and out-of-charset shapes.
    #[test]
    fn parse_owner_repo_accepts_real_slugs_rejects_path_smuggling() {
        assert_eq!(
            parse_owner_repo("qontinui/qontinui-runner"),
            Some(("qontinui", "qontinui-runner"))
        );
        assert_eq!(parse_owner_repo("a/b.c_d-e"), Some(("a", "b.c_d-e")));
        // Rejections: no slash, extra path segments, dot-dot, leading
        // punctuation, empty segments, whitespace, percent-encoding.
        assert_eq!(parse_owner_repo("no-slash"), None);
        assert_eq!(parse_owner_repo("a/b/c"), None);
        assert_eq!(parse_owner_repo("../etc/passwd"), None);
        assert_eq!(parse_owner_repo("a/.."), None);
        assert_eq!(parse_owner_repo("a/."), None);
        assert_eq!(parse_owner_repo("-lead/repo"), None);
        assert_eq!(parse_owner_repo("owner/.hidden"), None);
        assert_eq!(parse_owner_repo("/repo"), None);
        assert_eq!(parse_owner_repo("owner/"), None);
        assert_eq!(parse_owner_repo("a b/c"), None);
        assert_eq!(parse_owner_repo("a%2f/c"), None);
    }

    /// The upstream URL is the FIXED coord route template with the validated
    /// segments interpolated — no double slash on a trailing-slash base.
    #[test]
    fn vcs_pr_upstream_url_builds_fixed_coord_route() {
        assert_eq!(
            vcs_pr_upstream_url("https://coord.example.test", "qontinui", "qontinui-runner"),
            "https://coord.example.test/coord/repos/qontinui/qontinui-runner/pull-requests"
        );
        assert_eq!(
            vcs_pr_upstream_url("http://127.0.0.1:9870/", "a", "b"),
            "http://127.0.0.1:9870/coord/repos/a/b/pull-requests"
        );
    }

    /// The forwarded body is the inbound body MINUS `repo`, with absent
    /// optional fields omitted (not nulled) and present ones carried through.
    #[test]
    fn vcs_pr_upstream_body_strips_repo_and_omits_absent_options() {
        let minimal = VcsPullRequestBody {
            repo: "o/r".to_string(),
            head: "feat/x".to_string(),
            base: None,
            title: "feat: x".to_string(),
            body: None,
            draft: None,
        };
        let v = vcs_pr_upstream_body(&minimal);
        assert_eq!(v, serde_json::json!({"head": "feat/x", "title": "feat: x"}));
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("repo"), "repo travels in the URL path");
        assert!(!obj.contains_key("base") && !obj.contains_key("draft"));

        let full = VcsPullRequestBody {
            repo: "o/r".to_string(),
            head: "feat/x".to_string(),
            base: Some("main".to_string()),
            title: "feat: x".to_string(),
            body: Some("body text".to_string()),
            draft: Some(true),
        };
        assert_eq!(
            vcs_pr_upstream_body(&full),
            serde_json::json!({
                "head": "feat/x",
                "base": "main",
                "title": "feat: x",
                "body": "body text",
                "draft": true,
            })
        );
    }

    /// Absent `X-Coord-Mcp-Proxy-Key` → 401 with the route's own code, before
    /// any body parsing or upstream I/O.
    #[tokio::test]
    async fn missing_nonce_is_401_never_forwarded() {
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "VCS_PR_PROXY_UNAUTHORIZED");
    }

    /// A wrong (unregistered) nonce → the same 401.
    #[tokio::test]
    async fn wrong_nonce_is_401() {
        let _amb = crate::test_env::isolated_ambient();
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", "not-a-registered-nonce")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_UNAUTHORIZED");
    }

    /// A PR body that is not UTF-8 (the `0x97` em dash Git Bash curl sends)
    /// answers `INVALID_UTF8` with the encoding remedy, not the generic
    /// `VCS_PR_PROXY_BAD_REQUEST` deserialize text.
    #[tokio::test]
    async fn non_utf8_pr_body_answers_invalid_utf8() {
        let _amb = crate::test_env::isolated_ambient();
        let nonce = crate::coord_mcp::register_agent_proxy_nonce(
            "/tmp/vcs-pr-proxy-invalid-utf8-test",
            uuid::Uuid::new_v4(),
        );
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", &nonce)
                    .header("content-type", "application/json")
                    .body(Body::from(
                        &b"{\"repo\":\"a/b\",\"head\":\"h\",\"title\":\"a \x97 b\"}"[..],
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400);
        let v = body_json(resp).await;
        assert_eq!(v["code"], "INVALID_UTF8");
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("request body is not valid UTF-8"));
    }

    /// An AGENT-bound nonce is ACCEPTED by the principal gate (coord-spawned
    /// agent sessions are this feature's primary population — coord's upstream
    /// route takes agent JWTs too). With no live token slot registered for the
    /// agent (torn-down/restarted), the handler fails closed with the DISTINCT
    /// agent-gone 401 — proving it got PAST the old device-only rejection.
    #[tokio::test]
    async fn agent_nonce_passes_principal_gate_and_fails_closed_without_token_slot() {
        let _amb = crate::test_env::isolated_ambient();
        let agent_id = uuid::Uuid::new_v4();
        let nonce = crate::coord_mcp::register_agent_proxy_nonce(
            "/tmp/vcs-pr-proxy-agent-nonce-test",
            agent_id,
        );

        // A malformed body with a VALID agent nonce is a 400 — the shape is
        // validated before any credential lookup.
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", &nonce)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"not":"a pr body"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "agent nonce must reach body validation");
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_BAD_REQUEST");

        // A well-formed body proceeds to the agent-token lookup, which has no
        // slot for this agent → the distinct fail-closed 401 (NOT the generic
        // unrecognized-nonce 401, and NOT a forward).
        let resp = vcs_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/vcs/pull-requests")
                    .header("X-Coord-Mcp-Proxy-Key", &nonce)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"repo":"o/r","head":"h","title":"t"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
        let v = body_json(resp).await;
        assert_eq!(v["code"], "VCS_PR_PROXY_AGENT_GONE");
    }

    /// The base-resolution arm the VCS forwarder now reports in its 502s. Any
    /// arm would do for the transport tests below; naming one explicitly is
    /// what makes the field's presence assertable.
    const TEST_COORD_BASE_SOURCE: qontinui_runner_lib::profiles::CoordBaseSource =
        qontinui_runner_lib::profiles::CoordBaseSource::Env;

    /// The forwarding leg against a local mock coord: the bearer is injected as
    /// `Authorization: Bearer <token>`, the JSON body arrives verbatim, and
    /// coord's status + body come back unreshaped — including the honest
    /// non-2xx verdicts (403 repo-not-in-tenant, 429 rate limit).
    #[tokio::test]
    async fn forward_vcs_pr_post_injects_bearer_and_passes_through_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/coord/repos/{owner}/{repo}/pull-requests",
                post(
                    |axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
                     headers: axum::http::HeaderMap,
                     body: axum::body::Bytes| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        let body = String::from_utf8_lossy(&body).to_string();
                        (
                            axum::http::StatusCode::CREATED,
                            axum::Json(serde_json::json!({
                                "number": 42,
                                "url": format!("https://github.com/{owner}/{repo}/pull/42"),
                                "echo_auth": auth,
                                "echo_body": body,
                            })),
                        )
                    },
                ),
            )
            .route(
                "/coord/repos/other/denied/pull-requests",
                post(|| async {
                    (
                        axum::http::StatusCode::FORBIDDEN,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        r#"{"detail":"repo not in caller tenant"}"#,
                    )
                }),
            );
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        let base = format!("http://{addr}");

        // Happy path: 201 + coord's body verbatim, bearer injected.
        let url = vcs_pr_upstream_url(&base, "qontinui", "qontinui-runner");
        let body = serde_json::json!({"head": "feat/x", "title": "feat: x"});
        let resp =
            forward_vcs_pr_post(&url, "test-device-jwt", &body, TEST_COORD_BASE_SOURCE).await;
        assert_eq!(resp.status(), 201);
        let v = body_json(resp).await;
        assert_eq!(v["number"], 42);
        assert_eq!(
            v["url"],
            "https://github.com/qontinui/qontinui-runner/pull/42"
        );
        assert_eq!(v["echo_auth"], "Bearer test-device-jwt");
        assert_eq!(
            v["echo_body"],
            serde_json::to_string(&body).unwrap(),
            "the JSON body must arrive verbatim"
        );

        // Non-2xx coord verdict: status + body verbatim, not reshaped.
        // (The specific mock route wins over the {owner}/{repo} template.)
        let url = vcs_pr_upstream_url(&base, "other", "denied");
        let resp =
            forward_vcs_pr_post(&url, "test-device-jwt", &body, TEST_COORD_BASE_SOURCE).await;
        assert_eq!(resp.status(), 403);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            std::str::from_utf8(&bytes).unwrap(),
            r#"{"detail":"repo not in caller tenant"}"#,
            "coord's error body must come back verbatim"
        );
    }

    /// Coord unreachable → 502 from the runner with the distinct upstream code.
    #[tokio::test]
    async fn forward_vcs_pr_post_unreachable_coord_is_502() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let url = vcs_pr_upstream_url(&format!("http://127.0.0.1:{port}"), "o", "r");
        let resp = forward_vcs_pr_post(
            &url,
            "test-device-jwt",
            &serde_json::json!({"head": "h"}),
            TEST_COORD_BASE_SOURCE,
        )
        .await;
        assert_eq!(resp.status(), 502);
        let v = body_json(resp).await;
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "VCS_PR_PROXY_UPSTREAM_UNREACHABLE");

        // ---- ITEM 1: the 502 carries a MULTI-LEVEL source chain ------------
        //
        // A REAL refused connect, not a synthetic error: `reqwest`'s own
        // `Display` stops at "error sending request for url (…)", identical for
        // a refused connect, a DNS failure and a TLS verdict. The OS-level leaf
        // ("Connection refused (os error 111)" on Linux, "os error 10061" on
        // Windows) lives two `source()` hops below it, and reaching the caller
        // is the whole point of the item.
        let err = v["error"].as_str().expect("error is a string");
        assert!(
            err.starts_with("coord PR endpoint unreachable: "),
            "the leading prose is compat-preserved: {err}"
        );
        assert!(
            err.matches(": ").count() >= 3,
            "at least THREE `: ` joins — the prefix, the reqwest head, and one \
             real cause below it. A single-level render is the defect: {err}"
        );
        let lower = err.to_ascii_lowercase();
        assert!(
            lower.contains("os error") || lower.contains("connection refused"),
            "the OS-level leaf must reach the caller, not be swallowed by a \
             bare `{{e}}`: {err}"
        );

        // ---- ITEM 1 (parity): the two fields this door alone lacked --------
        assert_eq!(v["upstream_url"], url);
        assert_eq!(
            v["coord_base_source"],
            TEST_COORD_BASE_SOURCE.as_str(),
            "a misconfigured-upstream 502 must say where the base came from"
        );

        // ---- ITEM 2: the egress discriminator ------------------------------
        let eg = &v["egress"];
        assert_eq!(eg["client"], "vcs-pr-proxy");
        for k in [
            "process_uptime_ms",
            "open_handles",
            "socket_handles",
            "in_flight",
            "failures_total",
            "pool_introspection",
        ] {
            assert!(eg.get(k).is_some(), "missing egress.{k} in {v}");
        }
        assert!(
            eg["failures_total"].as_u64().unwrap_or(0) >= 1,
            "the failure that produced THIS envelope must already be counted"
        );
        // A count that could not be taken is a typed object, never a `0`.
        for k in ["open_handles", "socket_handles"] {
            let m = &eg[k];
            assert!(
                m.is_u64() || m.get("unavailable").is_some(),
                "egress.{k} must be a count or a typed unavailable, got {m}"
            );
        }
    }
}
