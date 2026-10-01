//! `GET /glossary` — the product glossary this runner build was compiled with.
//!
//! Plan `2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists`,
//! Phase C2 (runner half).
//!
//! # One source, served
//!
//! The glossary is `qontinui_types::glossary`, generated from
//! `qontinui-schemas/glossary/terms.toml` and compiled into this binary. This
//! route serves that table and nothing else — no database, no coord round-trip,
//! no file on disk — so it answers identically on a machine that has never
//! been paired, is offline, or has no workspace at all.
//!
//! - `GET /glossary` → `200 {version, source: "embedded", build_id,
//!   content_sha256, terms: [..]}`, every term in glossary order.
//! - `GET /glossary?term=<id>` → the same envelope with `terms` holding exactly
//!   that one entry.
//! - An id this version does not define → `404` carrying a
//!   [`qontinui_types::refusal::Refusal`] (`code: "glossary_term_unknown"`)
//!   plus `known_version`.
//!
//! # Three distinct answers, never collapsed
//!
//! A `404 glossary_term_unknown` naming `known_version`, a `200` whose `terms`
//! is the full table, and a transport failure are three different statements.
//! In particular an unknown id never degrades to `200 {terms: []}` — an empty
//! list would read as "this version has no such term, and no terms at all".
//!
//! # Reachability
//!
//! The glossary is public product vocabulary: it returns no secret, reads or
//! writes no caller-named path, makes no outbound request and drives no
//! process, so it is not a door. It is on
//! [`crate::mcp::origin_guard::FOREIGN_ROUTES`] so every local caller reaches
//! it without a credential in every route policy — an agent, a script, the
//! runner's own webview, the ui-bridge extension and any web page alike. The
//! remote `http_request` relay is a separate allowlist
//! (`relay_path_policy::RELAY_ALLOWED`) and does not reach it.
//!
//! # The 404 body is not an `ApiResponse`
//!
//! It is the schemas `Refusal` envelope (plus `known_version`), which carries
//! no `success` field, so `envelope_rewrite_middleware`'s JSON pass classifies
//! it as foreign and leaves it byte-for-byte as built here (pinned by
//! `unknown_term_survives_the_envelope_layer`).

use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use qontinui_types::glossary::{
    lookup, GlossaryEntry, GlossaryTerm, GLOSSARY, GLOSSARY_CONTENT_SHA256, GLOSSARY_VERSION,
};
use qontinui_types::refusal::{NextAction, NextActionKind, Refusal, RefusalCode, RefusalSource};

use crate::mcp::types::ApiState;

/// The only `source` this runner serves: the table compiled into the binary.
pub const SOURCE_EMBEDDED: &str = "embedded";

/// How much of an unknown id the 404 echoes back.
const MAX_ECHOED_ID_CHARS: usize = 128;

/// Query for `GET /glossary`.
///
/// `deny_unknown_fields`: a mistyped key (`?terms=gate`, `?id=gate`) is a 400,
/// never a silent whole-glossary 200, as coord's door does (the 400 body
/// is this runner's `ApiResponse` envelope, not coord's).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryQuery {
    /// A stable glossary id (`gate`, `work_unit`, …). Absent → every term.
    #[serde(default)]
    pub term: Option<String>,
}

/// The `200` body.
#[derive(Debug, Serialize)]
pub struct GlossaryResponse {
    /// [`GLOSSARY_VERSION`] of the compiled-in table.
    pub version: u32,
    /// Always [`SOURCE_EMBEDDED`].
    pub source: &'static str,
    /// `RUNNER_BUILD_ID` of this binary, the same value `/health` serves as
    /// `buildId`, so two surfaces showing different text can be attributed.
    pub build_id: &'static str,
    /// [`GLOSSARY_CONTENT_SHA256`] — the digest of the canonical content.
    pub content_sha256: &'static str,
    /// The requested term, or every term in glossary order.
    pub terms: Vec<&'static GlossaryEntry>,
}

/// The `404` body: the schemas refusal envelope plus the glossary version the
/// id was looked up against.
#[derive(Debug, Serialize)]
pub struct GlossaryTermUnknownBody {
    #[serde(flatten)]
    pub refusal: Refusal,
    /// The glossary version that does not define the requested id.
    pub known_version: u32,
}

fn envelope(terms: Vec<&'static GlossaryEntry>) -> GlossaryResponse {
    GlossaryResponse {
        version: GLOSSARY_VERSION,
        source: SOURCE_EMBEDDED,
        build_id: env!("RUNNER_BUILD_ID"),
        content_sha256: GLOSSARY_CONTENT_SHA256,
        terms,
    }
}

/// The refusal for an id this version does not define.
///
/// `fix_request` targeting the `term` parameter: sending the same id again
/// fails the same way, and the valid ids are listed in `detail` so the caller
/// can correct it without a second request.
pub fn unknown_term_refusal(id: &str, observed_at: String) -> GlossaryTermUnknownBody {
    let known = GlossaryTerm::ALL
        .iter()
        .map(|t| t.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let base = Refusal::new(
        RefusalCode::GlossaryTermUnknown,
        NextAction::new(NextActionKind::FixRequest).with_target("term"),
        RefusalSource::Runner,
        observed_at,
    );
    let refusal = if id.is_empty() {
        // No discriminator to name: say the id was empty rather than quoting "".
        base.with_detail(format!(
            "the term id was empty; glossary version {GLOSSARY_VERSION} defines: {known}"
        ))
    } else {
        // The caller's id is echoed, bounded so a huge query cannot bloat the body.
        let shown: String = id.chars().take(MAX_ECHOED_ID_CHARS).collect();
        base.with_discriminator(shown.clone()).with_detail(format!(
            "{shown:?} is not defined by glossary version {GLOSSARY_VERSION}; defined ids: {known}"
        ))
    };
    GlossaryTermUnknownBody {
        refusal,
        known_version: GLOSSARY_VERSION,
    }
}

/// Resolve a query to its response. Pure apart from the timestamp, so the
/// whole contract is testable without a router.
pub fn answer(term: Option<&str>) -> Response {
    match term {
        None => Json(envelope(GLOSSARY.iter().collect())).into_response(),
        Some(id) => match lookup(id) {
            Some(entry) => Json(envelope(vec![entry])).into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(unknown_term_refusal(id, chrono::Utc::now().to_rfc3339())),
            )
                .into_response(),
        },
    }
}

/// `GET /glossary` (+ `?term=<id>`).
pub async fn glossary_handler(Query(q): Query<GlossaryQuery>) -> Response {
    answer(q.term.as_deref())
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new().route("/glossary", get(glossary_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use serde_json::Value;
    use tower::ServiceExt;

    async fn body_json(resp: Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    fn app() -> Router {
        Router::new()
            .route("/glossary", get(glossary_handler))
            .layer(axum::middleware::from_fn(
                crate::mcp::envelope::envelope_rewrite_middleware,
            ))
    }

    async fn get_path(path: &str) -> (StatusCode, Value) {
        let resp = app()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        (status, body_json(resp).await)
    }

    /// The served version, digest and roster ARE the embedded constants —
    /// the acceptance clause "the doors return the same `version` for the same
    /// build" on the runner's side.
    #[tokio::test]
    async fn served_version_equals_the_embedded_constant() {
        let (status, body) = get_path("/glossary").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["version"], GLOSSARY_VERSION);
        assert_eq!(body["source"], "embedded");
        assert_eq!(body["build_id"], env!("RUNNER_BUILD_ID"));
        assert_eq!(body["content_sha256"], GLOSSARY_CONTENT_SHA256);
        let terms = body["terms"].as_array().expect("terms array");
        assert_eq!(terms.len(), GLOSSARY.len());
        for (served, entry) in terms.iter().zip(GLOSSARY) {
            assert_eq!(served["id"], entry.id.as_str());
            assert_eq!(served["short"], entry.short);
            assert_eq!(served["long"], entry.long);
            assert_eq!(served["since"], entry.since);
        }
    }

    #[tokio::test]
    async fn one_term_is_served_byte_for_byte() {
        let (status, body) = get_path("/glossary?term=gate").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["version"], GLOSSARY_VERSION);
        let terms = body["terms"].as_array().expect("terms array");
        assert_eq!(terms.len(), 1);
        let gate = GlossaryTerm::Gate.entry();
        assert_eq!(terms[0]["id"], "gate");
        assert_eq!(terms[0]["term"], gate.term);
        assert_eq!(terms[0]["short"], gate.short);
        let see_also: Vec<&str> = gate.see_also.iter().map(|t| t.as_str()).collect();
        assert_eq!(terms[0]["see_also"], serde_json::json!(see_also));
    }

    /// An unknown id is a typed 404, never `200 {terms: []}`, and it survives
    /// the envelope layer unchanged (no `success`, no rewritten `code`).
    #[tokio::test]
    async fn unknown_term_survives_the_envelope_layer() {
        let (status, body) = get_path("/glossary?term=no_such_term").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "glossary_term_unknown");
        assert_eq!(body["known_version"], GLOSSARY_VERSION);
        assert_eq!(body["discriminator"], "no_such_term");
        assert_eq!(body["source"], "runner");
        assert_eq!(body["next_action"]["kind"], "fix_request");
        assert_eq!(body["next_action"]["target"], "term");
        assert!(body.get("success").is_none(), "{body}");
        assert!(body.get("terms").is_none(), "{body}");
        assert!(body["detail"].as_str().unwrap().contains("gate"));
        // The body is a real Refusal: it decodes, and renders a non-empty
        // sentence naming what to do.
        let decoded: Refusal = serde_json::from_value(body.clone()).expect("a Refusal");
        assert_eq!(decoded.code, RefusalCode::GlossaryTermUnknown);
        assert!(decoded
            .render()
            .starts_with("That term is not in this version's glossary"));
    }

    #[tokio::test]
    async fn an_empty_term_is_an_unknown_term_not_the_whole_table() {
        let (status, body) = get_path("/glossary?term=").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "glossary_term_unknown");
        assert!(body.get("discriminator").is_none(), "{body}");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .starts_with("the term id was empty"));
    }

    /// A mistyped query key is refused, never answered with the whole table.
    #[tokio::test]
    async fn a_mistyped_query_key_is_a_400_not_the_whole_table() {
        let (status, body) = get_path("/glossary?terms=gate").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.get("terms").is_none(), "{body}");
        // The extractor's rejection went through the envelope layer.
        assert_eq!(body["success"], false, "{body}");
        // `code_for_status(400)` is the historical `INVALID_JSON` for every 400.
        assert_eq!(body["code"], "INVALID_JSON", "{body}");
        assert!(body["error"].as_str().unwrap().contains("terms"), "{body}");
    }

    /// The echoed id is quoted, so a stray space in it is visible.
    #[test]
    fn the_unknown_id_is_echoed_quoted() {
        let v = serde_json::to_value(unknown_term_refusal(
            "gate ",
            "2026-10-01T00:00:00Z".to_string(),
        ))
        .unwrap();
        assert!(v["detail"]
            .as_str()
            .unwrap()
            .starts_with("\"gate \" is not defined"));
    }

    #[test]
    fn an_oversized_id_is_echoed_bounded() {
        let long = "x".repeat(10_000);
        let body = unknown_term_refusal(&long, "2026-10-01T00:00:00Z".to_string());
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v["discriminator"].as_str().unwrap().chars().count(),
            MAX_ECHOED_ID_CHARS
        );
    }

    /// Every id the table defines resolves through the route.
    #[test]
    fn every_defined_id_resolves() {
        for t in GlossaryTerm::ALL {
            assert_eq!(answer(Some(t.as_str())).status(), StatusCode::OK, "{t}");
        }
    }
}
