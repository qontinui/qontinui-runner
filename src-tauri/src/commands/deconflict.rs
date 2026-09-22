//! Tauri commands behind the in-session deconflict surfaces.
//!
//! - `resolve_escalation` — the deconflict advisory banner
//!   (`src/components/terminal/DeconflictAdvisoryBanner.tsx`) listens for the
//!   `coordinator-decision-created` event the `crate::deconflict` loop emits
//!   and dismisses an advisory through this command, which is the same
//!   `project.coordinator_decisions` row update the deleted Coordinator
//!   dashboard's Escalations panel used.
//! - `list_overlapping_intents` — the overlapping-intents panel: pairs of
//!   active agents whose declared overlap paths intersect, read from coord.
//!
//! Both were rehomed from the deleted `commands::productivity` by Phases 3
//! and 4 of `2026-09-12-consolidate-local-orchestration-onto-conductor`; the
//! command names are unchanged so the frontend needs no edit.

use std::time::Duration;

use serde::Serialize;
use tracing::warn;

use crate::commands::require_app_state;

/// Mark an escalation resolved with a free-form `resolution` note. Returns
/// `true` if a row was updated (i.e. the decision existed and wasn't
/// already resolved).
#[tauri::command]
pub async fn resolve_escalation(
    app_handle: tauri::AppHandle,
    decision_id: String,
    resolution: String,
) -> Result<bool, String> {
    let app_state = require_app_state(&app_handle)?;
    app_state
        .pg_db
        .resolve_coordinator_decision(&decision_id, &resolution)
        .await
}

// ============================================================================
// Coordination Phase 1B (§4.10) — Overlapping intents panel
//
// Coord owns this answer end to end. `coord.agent_worktrees` is authored by
// COORD, server-side, in `POST /agents/allocate`; alembic (qontinui-web) is
// the sole author of the `coord.*` SCHEMA and never runs on an end-user's
// box, where the runner's Postgres is a private embedded cluster. So the
// table is simply absent there — and a local copy of it would be a table
// coord never writes to, i.e. permanently empty. The panel therefore reads
// coord over HTTP rather than SQL-ing a mirror that cannot exist
// (plan `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`,
// P2c). The pairwise overlap computation moved with it: coord already ran
// `detect_overlap` on every intent write, so the runner was recomputing an
// answer coord had.
// ============================================================================

/// Cap on the live-agent set the panel asks coord to consider when the
/// frontend does not pass one. Coord clamps this to its own hard maximum.
const OVERLAPPING_INTENTS_DEFAULT_LIMIT: i64 = 200;

/// Deadline for the panel's coord read. Bounded on purpose: an unreachable
/// coord must leave the panel empty promptly, never stall the dashboard.
/// Matches the 5s the neighbouring fleet-health read uses.
const OVERLAPPING_INTENTS_TIMEOUT: Duration = Duration::from_secs(5);

/// One pair of agents whose `declared_overlap_paths` intersect, as coord
/// computed it.
///
/// The wire shape is coord's `agent_worktrees::OverlappingIntentPair`
/// verbatim (both sides are `camelCase`), and this type re-serializes it to
/// the frontend unchanged — the Productivity panel's contract is the same
/// as before the read moved to coord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlappingIntentPair {
    pub agent_a: String,
    pub agent_b: String,
    pub intent_a: Option<String>,
    pub intent_b: Option<String>,
    pub overlapping_paths: Vec<String>,
}

/// Coord's response envelope. Only `pairs` is load-bearing here; coord also
/// reports `count` / `agentsConsidered` / `agentsTruncated` / `truncated`,
/// which the panel does not render today. `#[serde(default)]` means a body
/// without the key deserializes to an empty panel rather than an error.
#[derive(Debug, Default, serde::Deserialize)]
struct OverlappingIntentsResponse {
    #[serde(default)]
    pairs: Vec<OverlappingIntentPair>,
}

/// The coord URL this panel reads. Split out so the query-string contract
/// (the `limit` coord clamps) is testable without a live coord.
fn overlapping_intents_url(base: &str, limit: i64) -> String {
    format!(
        "{}/coord/agent-worktrees/overlapping-intents?limit={limit}",
        base.trim_end_matches('/')
    )
}

/// GET the pairs from a KNOWN coord base. `Err` is any reason the answer
/// could not be obtained — no client, transport failure, non-2xx (including
/// a 403 from coord's fail-closed `FleetPrincipal` gate on an unpaired
/// runner), or an unparseable body. The caller decides what a failure means
/// for the panel; this fn never panics and never blocks past
/// [`OVERLAPPING_INTENTS_TIMEOUT`].
async fn fetch_overlapping_intents(
    base: &str,
    limit: i64,
) -> Result<Vec<OverlappingIntentPair>, String> {
    // The process-wide coord client — one connection pool, one resolver. It
    // carries no global timeout by design, so set the deadline here.
    let Some(client) = crate::coord_http::coord_client() else {
        return Err("shared coord HTTP client unavailable".to_string());
    };
    let resp = crate::coord_http::coord_get(client, overlapping_intents_url(base, limit))
        .timeout(OVERLAPPING_INTENTS_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("GET overlapping-intents: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("coord returned HTTP {}", status.as_u16()));
    }
    let body: OverlappingIntentsResponse = resp
        .json()
        .await
        .map_err(|e| format!("parse overlapping-intents: {e}"))?;
    Ok(body.pairs)
}

/// Degradation core: turn "the coord base, if this runner has one" into the
/// panel's rows. Never fails — every arm that cannot produce an answer
/// produces an EMPTY panel and a log line, because the Productivity
/// dashboard must keep rendering when coord does not answer.
///
/// The governing precedent is `fleet::publish_on_startup`: a coord outage
/// degrades the feature, never the boot.
///
/// - `None` base — the runner is ISOLATED (no `COORD_HTTP_URL`, no profile
///   `coord_url`, not a hosted tier). Not an error, not worth a `warn!`:
///   a standalone install has no fleet to overlap with. `debug!`.
/// - `Err` from the fetch — coord is configured but did not answer (down,
///   unreachable, 403 from an unpaired runner, garbage body). That IS worth
///   a `warn!`, and still leaves the panel empty.
///
/// Split from the Tauri command so both arms are testable without touching
/// process env or standing up a coord.
async fn overlapping_intents_for_base(
    base: Option<String>,
    limit: i64,
) -> Vec<OverlappingIntentPair> {
    let Some(base) = base else {
        tracing::debug!(
            "list_overlapping_intents: runner is isolated (no coord configured) — \
             the overlapping-intents panel stays empty"
        );
        return Vec::new();
    };
    match fetch_overlapping_intents(&base, limit).await {
        Ok(pairs) => pairs,
        Err(e) => {
            warn!(
                "list_overlapping_intents: coord read failed ({e}) — the \
                 overlapping-intents panel stays empty; the rest of the \
                 dashboard is unaffected"
            );
            Vec::new()
        }
    }
}

/// List unique unordered pairs of active agents whose declared
/// overlap-path sets intersect. Drives the Productivity dashboard's
/// "Overlapping intents" panel (Phase 1B §4.10).
///
/// Coord computes the pairs (`GET
/// /coord/agent-worktrees/overlapping-intents`), tenant-scoped from the
/// runner's device-JWT and bounded server-side. Each pair is reported once,
/// ordered by agent id, so `(agentA, agentB)` is stable across calls.
///
/// `limit` caps the live-agent set coord pairs over — the panel is
/// informational and bounding it keeps the dashboard cheap even at 300+
/// agents. Coord clamps it to its own hard maximum.
///
/// Always `Ok`: an isolated or unreachable coord yields an empty panel, not
/// an error dialog. See [`overlapping_intents_for_base`].
#[tauri::command]
pub async fn list_overlapping_intents(
    limit: Option<i64>,
) -> Result<Vec<OverlappingIntentPair>, String> {
    // `connected_coord_base` — the Option-family policy resolver, and the
    // right door here precisely because it can express ISOLATED. Its
    // String-family sibling `coord_base_with_source` always yields a base
    // (guessing dev-localhost when nothing is configured), which would have
    // this panel dial a phantom coord on every standalone install.
    let base = qontinui_runner_lib::profiles::connected_coord_base();
    let cap = limit.unwrap_or(OVERLAPPING_INTENTS_DEFAULT_LIMIT);
    Ok(overlapping_intents_for_base(base, cap).await)
}

#[cfg(test)]
mod overlap_tests {
    use super::*;

    /// Serve exactly ONE HTTP response on a fresh loopback listener, then hand
    /// back its base URL.
    ///
    /// The server deliberately reports NOTHING about delivery. It cannot: on
    /// Windows the RST is emitted at close, so `write_all` and `flush` both
    /// succeed on a connection the client will never read from. A "we served
    /// it" flag here was tried and measured useless — it stayed `true` through
    /// the mutation that destroys the response. Assert delivery on the CLIENT
    /// side or not at all.
    ///
    /// Two details here are load-bearing on Windows, and both were learned the
    /// hard way (CI run 32570533855, `test (windows-latest)`):
    ///
    /// 1. **The request is drained before the response is written.** Closing a
    ///    socket that still holds unread bytes in its receive queue makes TCP
    ///    send **RST instead of FIN**. The client's in-flight response is then
    ///    discarded and `reqwest` surfaces a transport error.
    /// 2. **The write half is shut down explicitly** so the peer sees a clean
    ///    FIN rather than inheriting whatever `drop` does.
    ///
    /// Why that turned into a red build rather than a flaky one:
    /// [`overlapping_intents_for_base`] swallows every transport error into an
    /// empty `Vec`. So an RST is indistinguishable from "coord answered with
    /// nothing" — it failed `a_healthy_coord_answer_reaches_the_panel`
    /// outright, and it made `a_non_2xx_from_coord_degrades_rather_than_erroring`
    /// pass for the WRONG REASON, asserting emptiness it would have observed
    /// even if the 403 arm had never run. Linux tolerated the same code, so
    /// this was invisible on `ubuntu-22.04`.
    async fn serve_one_response(response: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // (1) Drain the request head. A GET carries no body, so
                // end-of-headers is the whole request.
                let mut seen: Vec<u8> = Vec::new();
                let mut buf = [0u8; 1024];
                while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => seen.extend_from_slice(&buf[..n]),
                    }
                }
                let _ = sock.write_all(&response).await;
                let _ = sock.flush().await;
                // (2) Clean FIN.
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn url_carries_the_limit_and_normalizes_a_trailing_slash() {
        assert_eq!(
            overlapping_intents_url("https://coord.qontinui.io", 200),
            "https://coord.qontinui.io/coord/agent-worktrees/overlapping-intents?limit=200"
        );
        // A profile `coord_url` with a trailing slash must not produce `//`.
        assert_eq!(
            overlapping_intents_url("http://localhost:9870/", 7),
            "http://localhost:9870/coord/agent-worktrees/overlapping-intents?limit=7"
        );
    }

    #[test]
    fn parses_coords_pair_shape() {
        // Byte-for-byte the envelope coord's `get_overlapping_intents`
        // emits. A field rename on either side breaks this test rather than
        // silently emptying the panel in production.
        let body = r#"{
            "pairs": [
                {
                    "agentA": "0190000a-0000-7000-8000-000000000001",
                    "agentB": "0190000a-0000-7000-8000-000000000002",
                    "intentA": "refactor the executor",
                    "intentB": null,
                    "overlappingPaths": ["src-tauri/src/executor/mod.rs"]
                }
            ],
            "count": 1,
            "agentsConsidered": 2,
            "agentLimit": 200,
            "agentsTruncated": false,
            "truncated": false
        }"#;
        let parsed: OverlappingIntentsResponse =
            serde_json::from_str(body).expect("coord envelope must parse");
        assert_eq!(parsed.pairs.len(), 1);
        let p = &parsed.pairs[0];
        assert_eq!(p.agent_a, "0190000a-0000-7000-8000-000000000001");
        assert_eq!(p.agent_b, "0190000a-0000-7000-8000-000000000002");
        assert_eq!(p.intent_a.as_deref(), Some("refactor the executor"));
        assert_eq!(p.intent_b, None);
        assert_eq!(p.overlapping_paths, vec!["src-tauri/src/executor/mod.rs"]);
    }

    #[test]
    fn a_body_without_pairs_is_an_empty_panel_not_a_parse_error() {
        let parsed: OverlappingIntentsResponse =
            serde_json::from_str("{}").expect("a keyless body must still parse");
        assert!(parsed.pairs.is_empty());
    }

    #[test]
    fn pairs_reserialize_to_the_frontends_camel_case_contract() {
        // The Tauri command hands this straight to the React panel; the key
        // names are the contract that did NOT change when the read moved to
        // coord.
        let pair = OverlappingIntentPair {
            agent_a: "a".into(),
            agent_b: "b".into(),
            intent_a: Some("x".into()),
            intent_b: None,
            overlapping_paths: vec!["p".into()],
        };
        let v = serde_json::to_value(&pair).expect("serialize");
        assert_eq!(v["agentA"], "a");
        assert_eq!(v["agentB"], "b");
        assert_eq!(v["intentA"], "x");
        assert!(v["intentB"].is_null());
        assert_eq!(v["overlappingPaths"][0], "p");
    }

    #[tokio::test]
    async fn an_isolated_runner_gets_an_empty_panel() {
        // `connected_coord_base()` yields `None` on a standalone install.
        // That is a supported configuration, not a failure: empty panel, no
        // dial, no error.
        let pairs = overlapping_intents_for_base(None, 200).await;
        assert!(pairs.is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_coord_degrades_to_an_empty_panel_promptly() {
        let _amb = crate::test_env::isolated_ambient();
        // Port 1 on loopback refuses immediately; the assertion that matters
        // is that the failure DEGRADES (no panic, no `Err`, no hang) and
        // stays inside the bounded deadline.
        let started = std::time::Instant::now();
        let pairs = overlapping_intents_for_base(Some("http://127.0.0.1:1".to_string()), 200).await;
        assert!(pairs.is_empty());
        assert!(
            started.elapsed() < OVERLAPPING_INTENTS_TIMEOUT * 3,
            "an unreachable coord must not stall the dashboard: took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn a_non_2xx_from_coord_degrades_rather_than_erroring() {
        let _amb = crate::test_env::isolated_ambient();
        // Coord's gate is fail-closed: an unpaired runner gets 403
        // `auth_required`. The panel must treat that like any other
        // unavailable answer — empty, logged, never a dialog.
        //
        // This is asserted in TWO steps on purpose. `overlapping_intents_for_base`
        // collapses every failure into an empty Vec, so `is_empty()` alone is
        // VACUOUS — a reset connection satisfies it just as well as a 403, and
        // on Windows that is exactly what used to happen. Only the inner
        // `fetch_overlapping_intents` distinguishes them: a delivered non-2xx
        // yields `coord returned HTTP 403`, whereas a dead connection yields a
        // `GET overlapping-intents: …` transport error. Assert the status
        // first, THEN the panel contract.
        let body = br#"{"error":"auth_required"}"#;
        let response = || {
            format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes()
            .into_iter()
            .chain(body.iter().copied())
            .collect::<Vec<u8>>()
        };

        // (1) the 403 genuinely arrives and is read as a status, not a fault.
        let (base, server) = serve_one_response(response()).await;
        let err = fetch_overlapping_intents(&base, 200)
            .await
            .expect_err("a 403 must not parse as success");
        assert!(
            err.contains("403"),
            "expected the delivered status in the error, got {err:?} — a \
             transport error here means the response never arrived, which \
             would make step (2) vacuous"
        );
        server.abort();

        // (2) and the panel degrades to empty rather than surfacing it.
        let (base, server) = serve_one_response(response()).await;
        let pairs = overlapping_intents_for_base(Some(base), 200).await;
        assert!(pairs.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn a_healthy_coord_answer_reaches_the_panel() {
        let _amb = crate::test_env::isolated_ambient();
        // The happy path end-to-end over real HTTP: coord's envelope in,
        // the panel's rows out.
        let body = br#"{"pairs":[{"agentA":"a1","agentB":"a2","intentA":"i1","intentB":"i2","overlappingPaths":["src/x.rs"]}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body.iter().copied())
        .collect::<Vec<u8>>();
        let (base, server) = serve_one_response(response).await;
        let pairs = overlapping_intents_for_base(Some(base), 200).await;
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].agent_a, "a1");
        assert_eq!(pairs[0].overlapping_paths, vec!["src/x.rs"]);
        server.abort();
    }
}
