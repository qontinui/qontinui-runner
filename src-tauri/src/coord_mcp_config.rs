//! The `.mcp.json` coord-mcp **proxy-header contract**: which header names
//! carry the per-session loopback nonce, and the single pair of resolvers that
//! read it back out of a request or a config document.
//!
//! ## Why this is its own module
//!
//! Phase 2 of plan `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`
//! moved the nonce into `Authorization: Bearer <nonce>` (keeping the legacy
//! custom header). The dangerous half of that change was not the emitter — it
//! was the **readers**, five of which re-derived the header name from a
//! hardcoded literal and every one of which degraded *silently* on the new
//! shape (a `None`, a `continue`, an `unwrap_or("")`). One of those readers,
//! `coord_doctor`, is compiled into the **library** crate, while `coord_mcp`
//! is declared in `main.rs` only — so a resolver living in `coord_mcp` is
//! unreachable from it. This module is the shared home that makes "every
//! reader goes through one function" actually expressible.
//!
//! Declared in BOTH `lib.rs` and `main.rs`, like the other dual-rooted modules
//! (`auth`, `fs_perms`, `secure_storage`).

/// Header carrying the per-session loopback nonce that authenticates a
/// session's MCP client to the runner-local `/coord-mcp` proxy route.
/// Lowercase — HTTP header names are case-insensitive and axum's `HeaderMap`
/// keys are lowercased; the `.mcp.json` writer emits the canonical-case form.
pub const COORD_MCP_PROXY_KEY_HEADER: &str = "x-coord-mcp-proxy-key";

/// Canonical-case spelling of [`COORD_MCP_PROXY_KEY_HEADER`] as it appears as a
/// JSON key inside a `.mcp.json` `headers` object. HTTP lookups use the
/// lowercase constant (axum lowercases `HeaderMap` keys); JSON objects are
/// case-SENSITIVE, so the writer and every config reader need this spelling.
/// Pinned equal (modulo case) to the lowercase constant by a unit test.
pub const COORD_MCP_PROXY_KEY_HEADER_JSON: &str = "X-Coord-Mcp-Proxy-Key";

/// The standard `Authorization` header, as a JSON key in a `.mcp.json`
/// `headers` object.
///
/// **Why the proxy nonce now also travels here (plan
/// `2026-08-20-coord-mcp-reconnect-dcr-and-restart-orphaning`, Phase 2).** A
/// stale nonce 401s. Measured at client 2.1.236/2.1.237: an MCP client that
/// sees a 401 from an `http`-transport server whose *static* `headers` map has
/// no `Authorization` key attaches an OAuth provider (`hasAuthProvider: true`),
/// runs RFC 9728 → RFC 8414 discovery, finds nothing, and falls back to
/// Dynamic Client Registration at `<origin>/register` — which this runner 404s.
/// That failed DCR then writes a durable `mcpOAuth` entry into the client's
/// `.credentials.json`, after which the client sends the (now healthy) server
/// **zero** requests forever: `Skipping connection (cached needs-auth)`.
///
/// With a static `Authorization` present the client reports the connection as
/// failed and **never constructs an auth provider** (`hasAuthProvider: false`),
/// so no code path can mint a poison entry. The cache key is
/// `<serverName>|sha256({type,url,headers}).slice(0,16)` and the nonce lives
/// inside that hashed `headers` map, so before this change **every rotation
/// minted a new poison entry** — an unbounded accumulator (17 live
/// `coord-mcp` entries were measured on this box). Emitting `Authorization`
/// closes that class structurally.
pub const PROXY_AUTHORIZATION_HEADER_JSON: &str = "Authorization";

/// `Authorization` scheme prefix the proxy nonce travels under.
pub const PROXY_BEARER_PREFIX: &str = "Bearer ";

/// True iff `s` is JWT-shaped: three `.`-separated, non-empty segments.
///
/// The discriminator that keeps "accept the nonce from `Authorization`" from
/// swallowing the OTHER thing that legitimately lives in that header — a real
/// static bearer. A proxy nonce is two v4 UUID simple forms (64 hex chars, no
/// `.`), so the two shapes can never be confused. Every reader below uses this
/// so that:
///   * a static-bearer agent `.mcp.json` keeps classifying as a NON-proxy
///     shape (`read_proxy_nonce` → `None`, `coord doctor` → not-a-proxy), and
///   * a request that presents a genuine JWT in `Authorization` alongside a
///     proxy key in the custom header keeps authenticating off the custom key.
pub fn looks_like_jwt(s: &str) -> bool {
    let mut parts = s.split('.');
    let (a, b, c, extra) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next().is_some(),
    );
    !extra
        && matches!((a, b, c), (Some(a), Some(b), Some(c))
            if !a.is_empty() && !b.is_empty() && !c.is_empty())
}

/// Pull a proxy nonce out of an `Authorization` header VALUE, or `None` when
/// the value is not a nonce-shaped bearer (wrong scheme, empty, or a real JWT
/// — see [`looks_like_jwt`]).
pub fn proxy_nonce_from_authorization(value: &str) -> Option<&str> {
    let tok = value
        .strip_prefix(PROXY_BEARER_PREFIX)
        .or_else(|| value.strip_prefix("bearer "))?
        .trim();
    if tok.is_empty() || looks_like_jwt(tok) {
        return None;
    }
    Some(tok)
}

/// **THE request-side proxy-key resolver.** Every loopback proxy door
/// (`/coord-mcp`, the claims reads, the coord write forwarder, the VCS PR
/// route) resolves its nonce through here so the two accepted shapes can never
/// drift apart door-to-door.
///
/// `Authorization: Bearer <nonce>` is preferred; `X-Coord-Mcp-Proxy-Key` is the
/// legacy shape and stays accepted indefinitely, because `.mcp.json` files are
/// rewritten only on session spawn — never periodically — so configs written
/// before Phase 2 keep validating for as long as their sessions live.
pub fn proxy_nonce_from_request(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(n) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(proxy_nonce_from_authorization)
    {
        return Some(n.to_owned());
    }
    headers
        .get(COORD_MCP_PROXY_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// **THE config-side proxy-key resolver**, over a `.mcp.json` server entry's
/// `headers` OBJECT. Accepts both shapes (case-insensitively — JSON keys are
/// case-sensitive but hand-edited configs are not reliably canonical),
/// preferring `Authorization`.
///
/// A JWT in `Authorization` deliberately resolves to `None`: that is the
/// static-bearer (agent) shape, which the reconcile/self-heal path must never
/// treat as a proxy config.
pub fn proxy_nonce_from_header_object(headers: &serde_json::Value) -> Option<String> {
    let obj = headers.as_object()?;
    let get = |name: &str| {
        obj.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| v.as_str())
    };
    if let Some(n) = get(PROXY_AUTHORIZATION_HEADER_JSON).and_then(proxy_nonce_from_authorization) {
        return Some(n.to_owned());
    }
    get(COORD_MCP_PROXY_KEY_HEADER_JSON)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// [`proxy_nonce_from_header_object`] over a whole `.mcp.json` document.
pub fn proxy_nonce_from_config_doc(doc: &serde_json::Value) -> Option<String> {
    proxy_nonce_from_header_object(doc.pointer("/mcpServers/coord-mcp/headers")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    /// A real proxy nonce: two v4 UUID simple forms, 64 hex chars, no `.`.
    fn nonce() -> String {
        format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        )
    }

    /// A JWT-SHAPED string (three non-empty dot-separated segments). Only the
    /// shape matters to every discriminator in this module.
    const JWT_SHAPED: &str = "eyJhbGciOiJFZERTQSJ9.eyJzdWJfdHlwZSI6ImFnZW50In0.c2ln";

    /// The two spellings of the legacy header are the same name — the HTTP one
    /// is lowercase because axum lowercases `HeaderMap` keys, the JSON one is
    /// canonical-case because JSON object keys are case-SENSITIVE. Pinned so a
    /// later edit to one cannot silently fork them.
    #[test]
    fn json_and_http_spellings_of_the_legacy_header_are_the_same_name() {
        assert_eq!(
            COORD_MCP_PROXY_KEY_HEADER_JSON.to_ascii_lowercase(),
            COORD_MCP_PROXY_KEY_HEADER
        );
        assert_eq!(PROXY_BEARER_PREFIX, "Bearer ");
        assert_eq!(PROXY_AUTHORIZATION_HEADER_JSON, "Authorization");
    }

    /// The nonce-vs-JWT discriminator: the whole "accept the nonce from
    /// `Authorization`" change is safe only because these two shapes cannot be
    /// confused.
    #[test]
    fn looks_like_jwt_separates_a_bearer_token_from_a_proxy_nonce() {
        assert!(looks_like_jwt(JWT_SHAPED));
        assert!(looks_like_jwt("a.b.c"));
        assert!(!looks_like_jwt(&nonce()), "a 64-hex nonce has no dots");
        assert!(!looks_like_jwt("a.b"), "two segments is not a JWT");
        assert!(!looks_like_jwt("a.b.c.d"), "four segments is not a JWT");
        assert!(!looks_like_jwt("a..c"), "an empty segment is not a JWT");
        assert!(!looks_like_jwt(""));
    }

    #[test]
    fn authorization_yields_a_nonce_but_never_a_jwt_or_a_foreign_scheme() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_authorization(&format!("Bearer {n}")),
            Some(n.as_str())
        );
        // Lowercase scheme (some hand-written clients) still resolves.
        assert_eq!(
            proxy_nonce_from_authorization(&format!("bearer {n}")),
            Some(n.as_str())
        );
        // A genuine static bearer is NOT a proxy nonce — this is what keeps the
        // agent-path config classifying as a non-proxy shape.
        assert_eq!(
            proxy_nonce_from_authorization(&format!("Bearer {JWT_SHAPED}")),
            None
        );
        assert_eq!(proxy_nonce_from_authorization("Bearer "), None);
        assert_eq!(proxy_nonce_from_authorization(&format!("Basic {n}")), None);
        assert_eq!(proxy_nonce_from_authorization(&n), None);
    }

    // -- Request side --------------------------------------------------------

    fn req_headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    /// The Phase 2 shape authenticates on its own.
    #[test]
    fn request_resolves_the_nonce_from_authorization() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("authorization", &format!("Bearer {n}"))])),
            Some(n)
        );
    }

    /// The legacy shape keeps authenticating — every `.mcp.json` already on
    /// disk carries only that header, and configs are rewritten on session
    /// spawn, never periodically.
    #[test]
    fn request_resolves_the_nonce_from_the_legacy_custom_header() {
        let n = nonce();
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("x-coord-mcp-proxy-key", &n)])),
            Some(n)
        );
    }

    /// When both are present and DISAGREE, `Authorization` wins.
    #[test]
    fn request_prefers_authorization_when_both_are_present_and_disagree() {
        let auth = nonce();
        let legacy = nonce();
        assert_ne!(auth, legacy);
        let got = proxy_nonce_from_request(&req_headers(&[
            ("authorization", &format!("Bearer {auth}")),
            ("x-coord-mcp-proxy-key", &legacy),
        ]));
        assert_eq!(got, Some(auth));
    }

    /// ...but a genuine JWT in `Authorization` does NOT shadow a valid custom
    /// header. A caller that legitimately carries a bearer keeps authenticating
    /// off the proxy key rather than 401ing on its own bearer.
    #[test]
    fn request_falls_back_to_the_custom_header_when_authorization_is_a_real_jwt() {
        let legacy = nonce();
        let got = proxy_nonce_from_request(&req_headers(&[
            ("authorization", &format!("Bearer {JWT_SHAPED}")),
            ("x-coord-mcp-proxy-key", &legacy),
        ]));
        assert_eq!(got, Some(legacy));
    }

    #[test]
    fn request_with_no_recognised_header_resolves_none() {
        assert_eq!(proxy_nonce_from_request(&req_headers(&[])), None);
        assert_eq!(
            proxy_nonce_from_request(&req_headers(&[("authorization", "Bearer ")])),
            None
        );
    }

    // -- Config side ---------------------------------------------------------

    #[test]
    fn config_resolves_both_shapes_and_prefers_authorization() {
        let n = nonce();
        // Phase 2 shape (Authorization only).
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Legacy shape (custom header only) - every pre-Phase-2 file on disk.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Both, disagreeing -> Authorization wins.
        let other = nonce();
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{other}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n.clone()));

        // Header-name matching is case-insensitive - hand-edited configs are
        // not reliably canonical.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"x-coord-mcp-proxy-key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), Some(n));
    }

    /// The static-bearer (agent-path) config must keep reading as a NON-proxy
    /// shape. Getting this wrong would make the boot reconcile treat an agent
    /// config as one of ours and rewrite it - and would feed a JWT into the
    /// registry lookup as if it were a nonce.
    #[test]
    fn config_static_bearer_agent_shape_is_not_a_proxy_config() {
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://coord.example.test/mcp","headers":{{"Authorization":"Bearer {JWT_SHAPED}"}}}}}}}}"#
        ))
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
    }

    #[test]
    fn config_absent_or_empty_shapes_resolve_none() {
        let doc: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
        let doc: serde_json::Value =
            serde_json::from_str(r#"{"mcpServers":{"coord-mcp":{"headers":{}}}}"#).unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"X-Coord-Mcp-Proxy-Key":"  "}}}}"#,
        )
        .unwrap();
        assert_eq!(proxy_nonce_from_config_doc(&doc), None);
    }
}
