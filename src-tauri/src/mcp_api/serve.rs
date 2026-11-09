//! The HTTP server's edges: the unmatched-route and caught-panic responders,
//! and binding the listener. Moved verbatim out of `mcp_api.rs` (plan
//! `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`
//! Phase 2). `start_server` and the dedicated API runtime join this file once
//! the open PRs touching them have landed (D8).

/// Build the `NOT_FOUND` error message for an unmatched `(method, path)`.
///
/// For an unmatched path under the `/ui-bridge/` surface we append a discovery
/// hint pointing at the two self-describing endpoints (`/ui-bridge/_routes` for
/// the route manifest, `/ui-bridge/commands` for invokable commands) so a caller
/// that fat-fingered a route can find the real one without grepping the source.
/// When the path names a route the UI Bridge deliberately deleted, the message
/// first says so and names the replacement (table:
/// `mcp::ui_bridge::removed_routes::REMOVED_UI_BRIDGE_ROUTES`), then keeps the
/// discovery hint. Non-`/ui-bridge/` paths keep the bare message unchanged.
pub(super) fn not_found_message(method: &axum::http::Method, path: &str) -> String {
    let base = format!("No route for {} {}", method, path);
    if path.starts_with("/ui-bridge/") {
        let removed = crate::mcp::ui_bridge::removed_routes::removed_route_replacement(path)
            .map(|replacement| format!(" — this route was removed; use {replacement}"))
            .unwrap_or_default();
        format!(
            "{base}{removed} — discover routes at GET /ui-bridge/_routes, \
             invokable commands at GET /ui-bridge/commands"
        )
    } else {
        base
    }
}

/// Canonical JSON 404 handler for unmatched routes. See the `.fallback(...)`
/// call in `build_router` for why this exists (envelope universality + the
/// debug envelope_audit layer). For unmatched `/ui-bridge/*` paths the error
/// message also names the discovery endpoints — see `not_found_message`.
pub(super) async fn not_found_handler(
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    // Derived, never a repeated literal. This handler and the envelope layer
    // both answer for a 404, and when the literal lived here they disagreed:
    // an unmatched route said `NOT_FOUND` while a handler's own 404 was stamped
    // `BAD_REQUEST` by `code_for_status`'s catch-all. Sharing the derivation is
    // what makes that divergence unable to reopen — a test comparing two
    // literals would only have noticed it afterwards.
    let body = crate::mcp::types::ApiResponse::<()>::error_with_code(
        not_found_message(&method, uri.path()),
        crate::mcp::envelope::code_for_status(axum::http::StatusCode::NOT_FOUND),
    );
    (axum::http::StatusCode::NOT_FOUND, axum::Json(body)).into_response()
}

/// Convert a caught handler panic into a JSON 500 response that matches the
/// runner's `ApiResponse<()>` shape. Wired onto the main API router via
/// `tower_http::catch_panic::CatchPanicLayer::custom`.
///
/// The panic payload is `Box<dyn Any + Send>`; downcast to the two common
/// concrete types (`&'static str` from `panic!("literal")` and `String` from
/// `panic!("{}", x)`) and fall back to a generic message otherwise.
pub(super) fn runner_panic_handler(
    err: Box<dyn std::any::Any + Send + 'static>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let msg = if let Some(s) = err.downcast_ref::<&'static str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    };

    tracing::error!(handler_panic = true, message = %msg, "runner HTTP handler panicked");

    // The panic hook already wrote a `crash_*.txt` (it runs before unwinding and
    // cannot know the panic is about to be caught). We DID catch it — the
    // process lives and the caller gets a 500 envelope — so downgrade the dump.
    // Left as-is, the next startup's crash-dump scan would adopt it and report
    // `derived_status: errored` for a runner that never died.
    let retracted = crate::logging::retract_last_crash_dump("HTTP handler panic");
    tracing::debug!(
        retracted,
        "caught-panic crash-dump retraction after handler panic"
    );

    let body = crate::mcp::types::ApiResponse::<()>::error_with_code(
        format!("handler panicked: {}", sanitize_panic_message(&msg)),
        "INTERNAL_ERROR",
    );
    (
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(body),
    )
        .into_response()
}

/// What a caller is allowed to see of a caught panic payload.
///
/// The raw payload is whatever `panic!` / `unwrap` produced deep inside the
/// handler. On `GET /ui-bridge/analytics/health-score` that was a verbatim
/// tokio-postgres `db error: …` string — driver text and schema identifiers
/// on the wire to any UI Bridge caller, which is the closest thing to an
/// internal leak on the whole control surface. Callers get a stable marker
/// instead; the full payload is on the `tracing::error!` line immediately
/// above, which is where diagnosis belongs.
///
/// The `[envelope_audit]` prefix is the single passthrough. That message is
/// synthesised by the debug-only audit layer in this same crate — never user
/// input, never driver output — and naming the offending route to the caller
/// is its entire purpose.
fn sanitize_panic_message(msg: &str) -> String {
    if msg.starts_with("[envelope_audit]") {
        return msg.to_string();
    }
    "internal error (payload withheld — see the runner log for the panic details)".to_string()
}

/// Whether a bound socket address is reachable from the LAN: any
/// non-loopback IP counts (including the unspecified address `0.0.0.0`,
/// which listens on all interfaces); `127.0.0.1`/`::1` does not.
///
/// Used at bind time to populate `AppState.api_lan_bound`, which the
/// backend heartbeat advertises as `lan_reachable` (plan
/// 2026-06-12-mobile-account-usage-error-recovery, runner P1). Derived
/// from the listener's REAL local address so a future non-loopback bind
/// flips the advertisement without further changes.
pub(crate) fn lan_reachable_for_bound_addr(addr: &std::net::SocketAddr) -> bool {
    !addr.ip().is_loopback()
}

/// Try to bind to a port with SO_REUSEADDR
pub(super) fn try_bind_port(port: u16) -> Result<std::net::TcpListener, std::io::Error> {
    // Create socket with SO_REUSEADDR to allow binding even if there are zombie connections
    // This is necessary on Windows where TIME_WAIT/CLOSE_WAIT sockets can block port binding
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::STREAM,
        Some(socket2::Protocol::TCP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    // Bind loopback only: all consumers (Claude Code, supervisor, web frontend) are
    // co-located with the runner. Loopback traffic bypasses Windows Firewall, so
    // every uniquely-renamed `qontinui-runner-<id>.exe` copy avoids triggering a
    // first-run permission prompt. Also reduces attack surface — the API is never
    // exposed to LAN. Every URL the runner builds for its own API names this SAME
    // constant (`runner_loopback_base`), so the address a self-call dials cannot
    // drift from the address bound here.
    socket
        .bind(&std::net::SocketAddr::from((crate::mcp::types::RUNNER_API_BIND_IP, port)).into())?;
    socket.listen(1024)?;
    Ok(socket.into())
}

/// FINDING 3 — `/health` must not leak a tokio blocking thread per poll for
/// the whole duration of a UI-thread wedge.
///
/// `tokio::time::timeout(_, handle)` bounds the await, not the task: the
/// closure stays parked on tauri's untimed `getter!` until the loop resumes.
/// Measured at ~1 leaked thread per 3s of wedge per poller, i.e. ~25 minutes
/// of continuous wedge to exhaust tokio's 512-thread default — after which
/// every `spawn_blocking` in the process queues and a UI-thread wedge becomes
/// a full backend wedge.
#[cfg(test)]
mod not_found_envelope_tests {
    use super::not_found_handler;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode, Uri};
    use axum::Router;
    use tower::ServiceExt;

    /// The unmatched-route fallback must put `NOT_FOUND` on the wire — the
    /// same code the envelope layer derives for a handler's own 404.
    ///
    /// This is the end-to-end half of the one-status-one-code property. The
    /// handler no longer repeats a literal (it calls
    /// `envelope::code_for_status`), so the two cannot diverge by
    /// construction; this asserts the value that construction actually
    /// produces, driven through a real router rather than by re-calling the
    /// derivation and comparing it with itself.
    ///
    /// Before the fix these disagreed: an unmatched route answered
    /// `NOT_FOUND` while `code_for_status`'s catch-all stamped a handler's own
    /// 404 as `BAD_REQUEST`.
    #[tokio::test]
    async fn the_unmatched_route_fallback_emits_not_found() {
        let app = Router::new().fallback(not_found_handler);

        let req = Request::builder()
            .method(Method::GET)
            .uri(Uri::from_static("/no/such/route"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], serde_json::json!(false));
        assert_eq!(body["code"], "NOT_FOUND");
        assert_eq!(
            body["code"],
            crate::mcp::envelope::code_for_status(StatusCode::NOT_FOUND)
        );
    }
}

#[cfg(test)]
mod panic_catcher_tests {
    use super::runner_panic_handler;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    async fn panicking_handler() -> &'static str {
        panic!("intentional test panic");
    }

    #[tokio::test]
    async fn test_panic_caught_returns_500_json() {
        let app = Router::new().route("/boom", get(panicking_handler)).layer(
            tower_http::catch_panic::CatchPanicLayer::custom(runner_panic_handler),
        );

        let req = Request::builder().uri("/boom").body(Body::empty()).unwrap();
        let response = app.oneshot(req).await.unwrap();

        assert_eq!(response.status(), 500);
        let body_bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
        assert_eq!(body["success"], false);
        assert!(body["error"].as_str().unwrap().contains("panicked"));
    }
}

#[cfg(test)]
mod not_found_message_tests {
    use super::not_found_message;
    use axum::http::Method;

    #[test]
    fn ui_bridge_path_appends_discovery_hint() {
        let msg = not_found_message(&Method::GET, "/ui-bridge/elements");
        // Keeps the canonical prefix...
        assert!(msg.starts_with("No route for GET /ui-bridge/elements"));
        // ...and names both discovery endpoints so the hint can't dangle.
        assert!(msg.contains("GET /ui-bridge/_routes"));
        assert!(msg.contains("GET /ui-bridge/commands"));
    }

    #[test]
    fn non_ui_bridge_path_is_unchanged() {
        let msg = not_found_message(&Method::POST, "/api/v1/whatever");
        assert_eq!(msg, "No route for POST /api/v1/whatever");
    }

    /// An unknown path that was never a route keeps the plain discovery hint
    /// and does not claim a removal.
    #[test]
    fn unknown_ui_bridge_path_does_not_claim_removal() {
        let msg = not_found_message(&Method::POST, "/ui-bridge/control/screenshots");
        assert_eq!(
            msg,
            "No route for POST /ui-bridge/control/screenshots — discover routes at \
             GET /ui-bridge/_routes, invokable commands at GET /ui-bridge/commands"
        );
    }

    /// Every deliberately removed route's 404 says it was removed, names the
    /// replacement capture route and where its bytes live, and still carries
    /// the discovery hint after it.
    #[test]
    fn removed_ui_bridge_route_names_its_replacement() {
        use crate::mcp::ui_bridge::removed_routes::REMOVED_UI_BRIDGE_ROUTES;
        // Guards an emptied table, which would make the loop below vacuous. A
        // count floor went stale the first time an entry was dropped.
        assert!(!REMOVED_UI_BRIDGE_ROUTES.is_empty());
        for (removed, _) in REMOVED_UI_BRIDGE_ROUTES {
            let path = match removed.strip_suffix("/*") {
                Some(root) => format!("{root}/audit/accessibility"),
                None => (*removed).to_string(),
            };
            let msg = not_found_message(&Method::POST, &path);
            assert!(
                msg.starts_with(&format!(
                    "No route for POST {path} — this route was removed; use "
                )),
                "{path}: {msg}"
            );
            assert!(
                msg.contains("POST /ui-bridge/vision/capture"),
                "{path}: {msg}"
            );
            assert!(
                msg.contains("GET /ui-bridge/vision/cache/{sha256}"),
                "{path}: {msg}"
            );
            assert!(
                msg.ends_with(
                    "— discover routes at GET /ui-bridge/_routes, \
                     invokable commands at GET /ui-bridge/commands"
                ),
                "{path}: {msg}"
            );
        }
    }

    /// Route level: a removed route still answers 404 / `NOT_FOUND` through
    /// the real fallback handler — only the message text changed.
    #[tokio::test]
    async fn removed_route_404_keeps_status_and_code() {
        use axum::{body::Body, http::Request, Router};
        use tower::ServiceExt;
        let resp = Router::new()
            .fallback(super::not_found_handler)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/ui-bridge/control/screenshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 404);
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], "NOT_FOUND");
        let error = v["error"].as_str().unwrap();
        assert!(error.starts_with("No route for POST /ui-bridge/control/screenshot"));
        assert!(error.contains("POST /ui-bridge/vision/capture"), "{error}");
    }
}

#[cfg(test)]
mod panic_message_redaction_tests {
    use super::sanitize_panic_message;

    /// `GET /ui-bridge/analytics/health-score` returned
    /// `"handler panicked: <verbatim tokio-postgres db error>"` — driver text
    /// and schema identifiers on the wire to any UI Bridge caller, and the
    /// closest thing to an internal leak on the control surface.
    #[test]
    fn driver_text_never_reaches_the_caller() {
        let raw = "db error: ERROR: column \"health_score\" does not exist at                    tokio_postgres::connect_raw src/client.rs:412";
        let out = sanitize_panic_message(raw);
        assert!(!out.contains("tokio_postgres"), "leaked driver path: {out}");
        assert!(
            !out.contains("health_score"),
            "leaked schema identifier: {out}"
        );
        assert!(!out.contains("db error"), "leaked driver message: {out}");
        assert!(
            out.contains("runner log"),
            "must point at where detail lives"
        );
    }

    #[test]
    fn an_arbitrary_panic_payload_is_withheld() {
        let out = sanitize_panic_message("called `Option::unwrap()` on a `None` value");
        assert!(!out.contains("unwrap"), "leaked panic internals: {out}");
    }

    /// The debug-only envelope audit synthesises its own message naming the
    /// offending route, which is the whole point of it — never driver or user
    /// text, so it passes through.
    #[test]
    fn the_envelope_audit_marker_still_passes_through() {
        let msg = "[envelope_audit] GET /ui-bridge/foo returned 500 with non-JSON Content-Type";
        assert_eq!(sanitize_panic_message(msg), msg);
    }
}
