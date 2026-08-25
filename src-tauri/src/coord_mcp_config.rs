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

/// True iff the `coord-mcp` entry's `headers` object carries an `Authorization`
/// key at all — whatever its value.
///
/// This is a question about the **static headers map's SHAPE**, not about the
/// credential in it, and that is exactly the distinction the DCR escape turns
/// on: the MCP client's exemption predicate reads whether the static map has an
/// `Authorization` key, and attaches an OAuth provider when it does not. A
/// config that is otherwise perfectly healthy — right port, live registered
/// nonce — but carries only the legacy custom header is therefore still
/// DCR-escalating for the next client launched against it.
///
/// Used by the boot self-heal to tell "healthy AND non-escalating" (leave it)
/// from "healthy but still legacy-shaped" (rewrite in place, same nonce). See
/// `coord_mcp::RootReconcileAction::UpgradeHeaders`.
pub fn config_doc_has_static_authorization(doc: &serde_json::Value) -> bool {
    doc.pointer("/mcpServers/coord-mcp/headers")
        .and_then(|h| h.as_object())
        .map(|o| {
            o.keys()
                .any(|k| k.eq_ignore_ascii_case(PROXY_AUTHORIZATION_HEADER_JSON))
        })
        .unwrap_or(false)
}

/// JSON key of the **principal-class marker** stamped into the `coord-mcp`
/// `headers` object by the AGENT-path `.mcp.json` writer
/// (`coord_mcp::write_coord_mcp_agent_proxy_config`) — and by nothing else.
///
/// ## Why a marker exists at all
///
/// Three emitters produce a **byte-identical** proxy `.mcp.json` (they all
/// funnel through `coord_mcp::coord_mcp_proxy_config_json`), and their nonces
/// are three different security classes:
///
/// | emitter | principal | persisted | re-registered after a restart |
/// |---|---|---|---|
/// | `write_coord_mcp_proxy_config` | Device/Persistent | yes | usually |
/// | `write_coord_mcp_agent_proxy_config` | **Agent{id}** | never | **never — by design** |
/// | `provision_session_proxy_config` | Device/**Ephemeral** | never | **never — by design** |
///
/// Rows 2 and 3 are *guaranteed* to be unregistered after a restart, which is
/// exactly the predicate the boot adopt arm keys on
/// (`coord_mcp::ReconcileAction::AdoptNonce`). Adoption hard-codes
/// `principal: Device, lifetime: Persistent` — so without a marker the boot
/// reconcile would re-register an **agent-scoped** credential as a **device**
/// one, and the proxy would then inject the live DEVICE JWT for a nonce whose
/// whole point was to inject one agent's token. The ephemeral case is the same
/// shape: adoption would convert a TTL-bounded, opt-in-gated, never-persisted
/// credential into an unbounded persistent one.
///
/// The principal class is **not inferable** from the boot reconcile's inputs —
/// the file is byte-identical and a lifecycle record carries no principal-class
/// field — so the fix is to remove the unknowability at the SOURCE: the agent
/// writer self-identifies, and the reconcile refuses to touch what it cannot
/// vouch for. A legacy agent config written before this marker existed carries
/// nothing and is therefore still indistinguishable; that residual drains on its
/// own, because an agent config is rewritten at every agent spawn.
///
/// ## Why a header rather than a sibling field on the server object
///
/// The `headers` map is already an arbitrary string→string map that every MCP
/// client forwards verbatim to the server named in `url` — here, the runner's
/// OWN loopback `/coord-mcp` route, which ignores header names it does not
/// know. A new key beside `type`/`url`/`headers` would instead have to survive
/// whatever schema the client validates the server entry against, and a client
/// that rejects unknown keys would take coord-mcp away from every agent
/// session. The header is inert by construction; a sibling field is inert only
/// by assumption.
///
/// It carries no secret (the literal string `agent`), so emitting it costs
/// nothing even in `claude --debug mcp` output, where custom headers are
/// printed in the clear.
pub const COORD_MCP_PRINCIPAL_HEADER_JSON: &str = "X-Coord-Mcp-Principal";

/// The only value [`COORD_MCP_PRINCIPAL_HEADER_JSON`] is ever emitted with. The
/// DEVICE shape omits the header entirely rather than spelling a `device`
/// value — absence must keep meaning exactly what it meant before the marker
/// existed, so that not one already-written device config changes class.
pub const COORD_MCP_PRINCIPAL_AGENT: &str = "agent";

/// True iff the `coord-mcp` entry's `headers` object carries the
/// [`COORD_MCP_PRINCIPAL_HEADER_JSON`] marker naming the AGENT class.
///
/// Case-insensitive on both key and value, matching every other config reader
/// here (JSON keys are case-sensitive, hand-edited configs are not reliably
/// canonical). Absent / unparseable / any other value ⇒ `false`, which is the
/// pre-marker reading: **not marked is not proof of device class**, only proof
/// that this file cannot vouch for itself. Callers that need a safety property
/// must treat `true` as "refuse", never `false` as "permit anything".
pub fn config_doc_is_agent_marked(doc: &serde_json::Value) -> bool {
    doc.pointer("/mcpServers/coord-mcp/headers")
        .and_then(|h| h.as_object())
        .map(|o| {
            o.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case(COORD_MCP_PRINCIPAL_HEADER_JSON)
                    && v.as_str()
                        .map(|s| s.trim().eq_ignore_ascii_case(COORD_MCP_PRINCIPAL_AGENT))
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
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

    /// The header-SHAPE predicate the boot self-heal's upgrade-in-place arm
    /// keys on. It asks only whether the static map has the key — a legacy-only
    /// config is what leaves the next client DCR-escalating, regardless of how
    /// healthy its nonce is.
    #[test]
    fn static_authorization_presence_is_a_shape_question_not_a_credential_one() {
        let n = nonce();
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(
            !config_doc_has_static_authorization(&doc),
            "a legacy-only config is the DCR-escalating shape"
        );

        // Both shapes present (what the writer emits today).
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));

        // A JWT counts too — the predicate is about the KEY, not the value.
        let doc: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {JWT_SHAPED}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));

        // Case-insensitive, and absent shapes are false rather than a panic.
        let doc: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"authorization":"x"}}}}"#,
        )
        .unwrap();
        assert!(config_doc_has_static_authorization(&doc));
        let doc: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert!(!config_doc_has_static_authorization(&doc));
    }

    /// The agent principal marker: recognised case-insensitively on key AND
    /// value, absent on the device shape, and — the load-bearing part —
    /// invisible to every OTHER reader in this module, so stamping it cannot
    /// change how a config's port, nonce or header shape is classified.
    #[test]
    fn agent_principal_marker_is_recognised_and_inert_to_every_other_reader() {
        let n = nonce();
        let marked: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}","X-Coord-Mcp-Principal":"agent"}}}}}}}}"#
        ))
        .unwrap();
        assert!(config_doc_is_agent_marked(&marked));
        // Inert: the nonce and the header SHAPE read exactly as they do without it.
        assert_eq!(proxy_nonce_from_config_doc(&marked), Some(n.clone()));
        assert!(config_doc_has_static_authorization(&marked));

        // The DEVICE shape carries no marker — absence must keep meaning what it
        // meant before the marker existed.
        let device: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"headers":{{"Authorization":"Bearer {n}","X-Coord-Mcp-Proxy-Key":"{n}"}}}}}}}}"#
        ))
        .unwrap();
        assert!(!config_doc_is_agent_marked(&device));

        // Case-insensitive on key and value; whitespace-tolerant on the value.
        let odd: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"x-coord-mcp-principal":" AGENT "}}}}"#,
        )
        .unwrap();
        assert!(config_doc_is_agent_marked(&odd));

        // Any other value, and every absent shape, is NOT a marker.
        let other: serde_json::Value = serde_json::from_str(
            r#"{"mcpServers":{"coord-mcp":{"headers":{"X-Coord-Mcp-Principal":"device"}}}}"#,
        )
        .unwrap();
        assert!(!config_doc_is_agent_marked(&other));
        let none: serde_json::Value = serde_json::from_str(r#"{"mcpServers":{}}"#).unwrap();
        assert!(!config_doc_is_agent_marked(&none));
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
