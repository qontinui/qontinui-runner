//! The HTTP server's edges: the unmatched-route and caught-panic responders,
//! binding the listener, and serving it. Moved verbatim out of `mcp_api.rs` (plan
//! `2026-10-04-runner-mcp-api-rs-holds-the-http-composition-root-health-and-five-proxies-in-one-file`
//! Phase 2), together with the dedicated API runtime and `start_server`.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use tauri::{Emitter, Manager};
use tracing::{info, warn};

use super::create_router;
use crate::commands::rag::RAGState;
use crate::commands::AppState;

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

/// The address the API listener serves `port` on: `RUNNER_API_BIND_IP:port`,
/// bound by `listeners::bind_reuseaddr` (SO_REUSEADDR, non-blocking, backlog
/// 1024) or adopted from an inherited fd checked to be on exactly this address.
///
/// Loopback only: all consumers (Claude Code, supervisor, web frontend) are
/// co-located with the runner. Loopback traffic bypasses Windows Firewall, so
/// every uniquely-renamed `qontinui-runner-<id>.exe` copy avoids triggering a
/// first-run permission prompt. Also reduces attack surface — the API is never
/// exposed to LAN. Every URL the runner builds for its own API names this SAME
/// constant (`runner_loopback_base`), so the address a self-call dials cannot
/// drift from the address bound here.
fn api_addr(port: u16) -> std::net::SocketAddr {
    std::net::SocketAddr::from((crate::mcp::types::RUNNER_API_BIND_IP, port))
}

/// Acquire the API listener.
///
/// When an `api` fd was inherited (`inherited_api`), ONLY `port` is tried: the
/// socket the supervisor of this process (a systemd socket unit, or a
/// predecessor handing off) holds is the address every client dials, so moving
/// to `port + 1` would leave them queued on a socket nobody serves. Otherwise
/// `fallback_ports` (from `instance::api_ports_to_try`) are walked in order, as
/// they always were. `adopt_or_bind` is `listeners::adopt_or_bind` bound to
/// the `api` name in production; tests inject a store.
pub(super) fn acquire_api_listener_with(
    port: u16,
    fallback_ports: &[u16],
    inherited_api: bool,
    adopt_or_bind: impl Fn(
        std::net::SocketAddr,
    ) -> std::io::Result<qontinui_runner_lib::listeners::AcquiredListener>,
) -> Result<(qontinui_runner_lib::listeners::AcquiredListener, u16), std::io::Error> {
    let only_requested = [port];
    let ports: &[u16] = if inherited_api {
        &only_requested
    } else {
        fallback_ports
    };
    let mut last_error = None;
    for &try_port in ports {
        info!("MCP API server: adopt_or_bind(api, {})...", try_port);
        match adopt_or_bind(api_addr(try_port)) {
            Ok(acquired) => return Ok((acquired, try_port)),
            Err(e) => {
                warn!("Failed to bind to port {}: {}", try_port, e);
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("All ports failed")))
}

/// The answer to every request while the listener is adopted and the router is
/// not built yet: `503`, `Retry-After: 2`, `ApiResponse` envelope with code
/// `RUNNER_BOOTING`. Answering beats leaving the connection queued: a client
/// learns the runner is coming back instead of waiting out its own timeout.
pub(super) fn booting_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = crate::mcp::types::ApiResponse::<()>::error_with_code(
        "the runner is booting: it holds its listener but its routes are not ready yet — \
         retry after the Retry-After interval",
        qontinui_runner_lib::listeners::RUNNER_BOOTING_CODE,
    );
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "2")],
        axum::Json(body),
    )
        .into_response()
}

/// Where the real router lands once `start_server` has built it.
pub(super) type RouterSlot = Arc<std::sync::OnceLock<axum::Router>>;

/// The service an adopted listener serves from the moment it is adopted: every
/// request is answered [`booting_response`] until `slot` holds the real router,
/// and is handed to that router from then on. One connection may span the
/// swap (HTTP/1.1 keep-alive): each REQUEST reads the slot, so nothing is
/// dropped and nothing is re-dialled.
pub(super) fn booting_gate(slot: RouterSlot) -> axum::Router {
    axum::Router::new().fallback(move |req: axum::extract::Request| {
        let slot = slot.clone();
        async move {
            match slot.get() {
                Some(router) => {
                    let mut router = router.clone();
                    match tower::Service::call(&mut router, req).await {
                        Ok(response) => response,
                        Err(never) => match never {},
                    }
                }
                None => booting_response(),
            }
        }
    })
}

/// Offer the API listener to a self-managed upgrade (plan Phase 5): record
/// its fd in [`crate::runner_upgrade::listener_fds`] under `api`, the name the
/// successor adopts it by. Only a listener on the REQUESTED port is offered —
/// the successor's adoption checks the address, so handing it a fallback-port
/// socket would make the preflight promise a handoff the successor refuses.
/// The fd stays owned by the server; tokio's `from_std` keeps its number.
fn offer_api_listener_for_handoff(
    listener: &std::net::TcpListener,
    requested_port: u16,
    bound_port: u16,
) {
    if bound_port != requested_port {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        crate::runner_upgrade::listener_fds::register(
            qontinui_runner_lib::listeners::API_LISTENER_NAME,
            listener.as_raw_fd(),
        );
    }
    #[cfg(not(unix))]
    let _ = listener;
}

/// An API listener adopted at boot and already being served by the
/// [`booting_gate`], waiting for `start_server` to install the router.
struct EarlyApi {
    port: u16,
    local_addr: Option<std::net::SocketAddr>,
    slot: RouterSlot,
    served: tokio::task::JoinHandle<std::io::Result<()>>,
}

static EARLY_API: std::sync::Mutex<Option<EarlyApi>> = std::sync::Mutex::new(None);

fn take_early_api() -> Option<EarlyApi> {
    EARLY_API.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// When an `api` listener was inherited (socket activation, or a predecessor's
/// handoff), adopt it NOW and start answering it — every request gets
/// [`booting_response`] until `start_server` installs the router.
///
/// Called from `run_app` as soon as logging exists, long before the Tauri
/// setup reaches `start_server`: under a socket unit every connect made during
/// boot is already queued in this socket's backlog, and the earlier it is
/// accepted the sooner its client gets an answer. A plain launch (nothing
/// inherited) does nothing here, so the normal boot path is unchanged.
pub fn start_early_api_if_inherited() {
    use qontinui_runner_lib::listeners::{self, API_LISTENER_NAME};
    if !listeners::has_inherited(API_LISTENER_NAME) {
        return;
    }
    let port = crate::mcp::types::get_mcp_api_port();
    let listener = match listeners::take_inherited(API_LISTENER_NAME, api_addr(port)) {
        Ok(Some(listener)) => listener,
        Ok(None) => return,
        Err(e) => {
            warn!(
                port,
                error = %e,
                "MCP API server: the inherited `api` listener is unusable and was closed — \
                 the API will be bound at the usual point in boot"
            );
            return;
        }
    };
    let local_addr = listener.local_addr().ok();
    // An adopted `api` listener is on `api_addr(port)` by construction.
    offer_api_listener_for_handoff(&listener, port, port);
    let slot: RouterSlot = Arc::new(std::sync::OnceLock::new());
    let served = serve_on_dedicated_runtime(listener, booting_gate(slot.clone()), port);
    info!(
        port,
        "MCP API server: adopted the inherited `api` listener at boot; answering 503 \
         RUNNER_BOOTING until the router is ready"
    );
    *EARLY_API.lock().unwrap_or_else(|p| p.into_inner()) = Some(EarlyApi {
        port,
        local_addr,
        slot,
        served,
    });
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

/// Worker threads on the dedicated local-API runtime.
///
/// Four, not one: the local API multiplexes the supervisor's health probes,
/// the web frontend, Claude Code sessions and the coord-mcp proxy, and a
/// single worker turns one slow handler into a total outage — the exact shape
/// this runtime exists to prevent. Four is also small enough that the API can
/// never crowd out the app runtime it was split away from.
const API_RUNTIME_WORKER_THREADS: usize = 4;

/// The dedicated local-API runtime, kept alive for the process's lifetime.
///
/// It MUST outlive the call to [`serve_on_dedicated_runtime`] that builds it: dropping a
/// `Runtime` shuts its workers down, which would stop the server we just
/// started — and dropping one from inside an async context panics outright.
#[expect(
    clippy::disallowed_types,
    reason = "process-lived OnceLock static, never dropped, so outside the drop-from-async panic class; plan 2026-09-12-residual-work-from-the-april-2026-plan-audit"
)]
static API_RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

/// Serve the local API (`:9876`) on a tokio runtime of its **own**.
///
/// ## Why the API does not share the app runtime
///
/// Every other subsystem — the terminal bridge, the plan adapter, the backend
/// relay, the session store, every Tauri command — runs on the single app
/// runtime. One of them blocking its workers (a synchronous filesystem walk, a
/// `std::sync::Mutex` held across an `.await`, a saturated channel) takes the
/// HTTP API down WITH it, and takes it down in the worst possible way: the
/// listening socket still exists, so the OS completes the TCP handshake and the
/// connection sits in the accept backlog answering nothing. That is what the
/// 2026-08 incident looked like from outside — a process alive for 14 hours,
/// holding `:9876`, accepting connections, and returning zero bytes to three
/// consecutive 30-second probes, including on `/web-integration/status`.
///
/// Every remedy for that failure has to travel over the API — `/health`,
/// `/web-integration/status`, the supervisor's probes, `adb reverse` from the
/// mobile app. So the API is precisely the subsystem that must keep answering
/// while the rest of the process is sick, and it cannot do that from inside the
/// pool that is sick. Its own runtime means its workers are scheduled by the OS
/// independently of whatever the app runtime is stuck on.
///
/// This is not a claim that a wedge becomes harmless: a handler that blocks
/// still burns an API worker. It is the narrower, checkable claim that a wedge
/// **elsewhere** no longer silences the API — which is what turns a 14-hour
/// blind outage into a diagnosable one.
///
/// ## Why a `std::net::TcpListener` crosses the boundary
///
/// A `tokio::net::TcpListener` is registered with the I/O driver of the runtime
/// that created it. Building it here and moving it would leave it driven by the
/// app runtime — reintroducing the coupling this function removes, and silently.
/// So the *std* listener is what moves, and `from_std` runs on the far side.
fn serve_on_dedicated_runtime(
    std_listener: std::net::TcpListener,
    router: axum::Router,
    port: u16,
) -> tokio::task::JoinHandle<std::io::Result<()>> {
    // Refuse every connection whose peer process runs as a different OS user
    // (plan 2026-10-04-runner-loopback-api-refuses-other-local-users). Wrapped
    // INSIDE the spawned task, on the runtime that serves it, because the
    // guard's accept task is spawned on the runtime that wraps it.
    let serve = async move {
        let listener = tokio::net::TcpListener::from_std(std_listener)?;
        let listener =
            qontinui_runner_lib::peer_user_guard::GuardedListener::wrap(listener, "local-api")?;
        axum::serve(listener, router).await
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(API_RUNTIME_WORKER_THREADS)
        .enable_all()
        .thread_name("mcp-api-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // Fail OPEN. A degraded API that shares the app runtime is the
            // behaviour that shipped for years; no API at all is strictly
            // worse, and would make this hardening a new outage class.
            warn!(
                error = %e,
                "MCP API server: could not build the dedicated API runtime — falling \
                 back to the shared app runtime. The API will again go deaf if \
                 any other subsystem blocks the app runtime's workers."
            );
            return tauri::async_runtime::handle().inner().spawn(serve);
        }
    };

    // `Runtime::spawn` schedules onto `rt`'s workers from this thread (an app
    // runtime worker, or `run_app`'s thread on the early-adoption path), so no
    // raw thread handoff is needed and there is no window in which the
    // listener is owned by a thread that failed to start.
    let served = rt.spawn(serve);

    // Park the runtime in a process-lifetime home. `set` can only fail if the
    // API were served twice; forgetting the loser is deliberate — dropping it
    // here would shut down workers (and panic inside an async context) instead
    // of leaking one runtime in a case that cannot occur once.
    if let Err(loser) = API_RUNTIME.set(rt) {
        std::mem::forget(loser);
    }

    info!(
        port,
        workers = API_RUNTIME_WORKER_THREADS,
        "MCP API server: serving on a dedicated tokio runtime (isolated from the app runtime)"
    );
    served
}

/// Wait for a serve task started by [`serve_on_dedicated_runtime`].
async fn await_api_serve(
    served: tokio::task::JoinHandle<std::io::Result<()>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match served.await {
        Ok(r) => r.map_err(Into::into),
        Err(e) => Err(Box::new(std::io::Error::other(format!(
            "MCP API serve task ended abnormally: {e}"
        )))),
    }
}

/// Everything the runner records once it HOLDS its API listener, before any
/// route is served: the bound port in `AppState` and the install shim, the
/// LAN-reachability bit, the boot stage, and the loopback handshake key plus
/// the breadcrumb that names it.
fn record_api_listener(
    app_state: &AppState,
    requested_port: u16,
    bound_port: u16,
    local_addr: Option<std::net::SocketAddr>,
) {
    // Record whether the ACTUAL bound address is LAN-reachable (non-loopback).
    // Read from the listener rather than assumed, so the heartbeat's
    // `lan_reachable` advertisement stays honest if the bind strategy ever
    // changes. Currently always false: the API listens on 127.0.0.1 only
    // (intentional — see `api_addr`), and an adopted listener is checked
    // to be on exactly that address.
    let lan_bound = local_addr
        .map(|addr| lan_reachable_for_bound_addr(&addr))
        .unwrap_or(false);
    app_state.api_lan_bound.store(lan_bound, Ordering::Relaxed);
    if bound_port != requested_port {
        warn!(
            "Primary port {} was blocked, using fallback port {}. \
             Restart the app after zombie connections clear.",
            requested_port, bound_port
        );
    }
    info!("MCP API server listening on port {}", bound_port);

    // Store the actual bound port in AppState
    app_state.api_port.store(bound_port, Ordering::Relaxed);

    // Mirror the bound port into the install-interception shim's
    // process-global so the terminal env-seam (which has no `app_state`)
    // injects the CORRECT loopback port for secondary/temp runners — plan §6's
    // #1 live-verification footgun. Set HERE, the same place app_state.api_port
    // lands.
    crate::install_effects_producer::intercept::set_bound_port(bound_port);
    crate::boot_stage::stamp(crate::boot_stage::BootStage::ApiListenerBound);
    // Bind latency from the process clock, logged on every start so the
    // never-answered grace can be sized (and re-derived) from evidence, cold
    // boots included.
    let bind_latency_ms = crate::health_monitor::process_elapsed().as_millis() as u64;
    info!(
        port = bound_port,
        bind_latency_ms, "MCP API listener bound {bind_latency_ms} ms after process start"
    );

    // Rotate this runner start's SAME-USER loopback handshake key at the BOUND
    // port's path (`~/.qontinui/runner-loopback-key-<port>`) and then advertise
    // both the port and that path out-of-process in the breadcrumb record — the
    // key BEFORE the record that names it, so a reader that sees the record can
    // already read the key, and both BEFORE any route is served, so any caller
    // that can reach `/health` can already drive
    // `POST /coord-mcp/provision-session`. Keying on the bound port is what
    // stops a second runner on this box (which shares `~/.qontinui`) from
    // clobbering this one's key (plan
    // 2026-09-07-the-runner-loopback-handshake-key-is-box-global-and-a-second-runner-clobbers-it).
    // Synchronous and cheap; it never fails the boot — a write failure yields
    // `None`, the record says so, and the mint route denies every request
    // (fail-closed), which is a closed door, not an outage.
    let key_path = crate::coord_mcp::publish_loopback_handshake_key(bound_port);
    qontinui_runner_lib::runner_breadcrumb::publish(bound_port, key_path);
}

/// The API is serving routes: flip `api_ready`, tell systemd (`READY=1`), emit
/// `api-ready`, and title the window for a non-default port or a named
/// instance.
fn announce_api_ready(app_state: &AppState, emitter: &tauri::AppHandle, port: u16) {
    // Signal that the API is ready for requests
    app_state.api_ready.store(true, Ordering::Relaxed);
    // systemd `READY=1` half (plan 2026-10-19 Phase 2b); no-op without
    // `NOTIFY_SOCKET`.
    crate::sd_notify::mark_http_serving();
    if let Err(e) = emitter.emit("api-ready", port) {
        warn!("Failed to emit api-ready event: {}", e);
    } else {
        info!("Emitted api-ready event (port {})", port);
    }

    // Update window title if using non-default port or instance name
    let default_port = crate::mcp::types::MCP_API_PORT;
    let instance_name = crate::instance::instance_name();
    let needs_title_update = port != default_port || instance_name.is_some();
    if needs_title_update {
        let title = match instance_name {
            Some(name) => format!("Qontinui Runner — {} [:{}]", name, port),
            None => format!("Qontinui Runner [:{}]", port),
        };
        if let Some(window) =
            emitter.get_webview_window(qontinui_runner_lib::get_main_window_label())
        {
            if let Err(e) = window.set_title(&title) {
                warn!("Failed to set window title: {}", e);
            } else {
                info!("Window title set to: {}", title);
            }
        }
    }
}

/// Start the MCP API server
pub async fn start_server(
    app_state: Arc<AppState>,
    rag_state: Arc<RAGState>,
    app_handle: tauri::AppHandle,
    port: u16,
    instance_manager: Arc<crate::instance_manager::InstanceManager>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let emitter = app_handle.clone();
    let api_ready_flag = app_state.clone();

    // NOTE: this runner start's SAME-USER loopback handshake key is rotated in
    // `record_api_listener`, NOT here — it is keyed on the port this runner
    // ACTUALLY holds, which is unknown until the listener is acquired (a
    // blocked primary port silently promotes the runner onto `port + 1`).

    crate::boot_stage::stamp(crate::boot_stage::BootStage::ApiRouterBuild);
    info!("MCP API server: building router via create_router...");
    let router = create_router(app_state, rag_state, app_handle, instance_manager);
    info!("MCP API server: create_router returned, acquiring the listener");

    // The PG circuit breaker's prober needs a process-lived runtime: this one.
    // On a degraded boot (breaker Open, `cause=boot`) the prober starts now and
    // reconnects + provisions, lifting the pg_guard 503 when PG returns; on a
    // normal boot it starts on the first runtime trip.
    if let Some(pg) = crate::database::pg::PgDb::try_global() {
        if !crate::database::pg::pg_available() {
            tracing::warn!("Runner booted DEGRADED (no PG); starting the PG breaker's prober");
        }
        pg.start_background(tokio::runtime::Handle::current());
        crate::database::pg::health::spawn_health_refresher();
    }

    crate::boot_stage::stamp(crate::boot_stage::BootStage::ApiBindLoop);

    // An inherited listener adopted at boot is already being answered by the
    // booting gate: install the router behind it. No bind, no fallback port.
    if let Some(early) = take_early_api() {
        record_api_listener(&api_ready_flag, port, early.port, early.local_addr);
        if early.slot.set(router).is_err() {
            warn!("MCP API server: the booting gate already held a router — keeping it");
        }
        info!(
            port = early.port,
            "MCP API server: router installed behind the adopted listener; booting gate open"
        );
        announce_api_ready(&api_ready_flag, &emitter, early.port);
        return await_api_serve(early.served).await;
    }

    // Try the requested port first, then fallback ports if zombie connections
    // are blocking (Windows, after a crash leaves orphaned sockets) — unless an
    // `api` listener was inherited, which never falls back.
    use qontinui_runner_lib::listeners::{self, API_LISTENER_NAME};
    let (acquired, bound_port) = acquire_api_listener_with(
        port,
        &crate::instance::api_ports_to_try(port),
        // Not only an fd NAMED `api`: any inherited fd (or unreadable LISTEN_*
        // variables) means a socket unit may hold this port, and falling back
        // would put the runner on a port no client dials.
        listeners::has_inherited(API_LISTENER_NAME) || listeners::inherited_at_startup(),
        |addr| listeners::adopt_or_bind(API_LISTENER_NAME, addr),
    )?;
    let unclaimed = listeners::unclaimed_inherited();
    if !unclaimed.is_empty() {
        warn!(
            ?unclaimed,
            "MCP API server: inherited sockets no listener claimed stay open for the process \
             lifetime"
        );
    }
    info!(
        port = bound_port,
        origin = acquired.origin.as_str(),
        "MCP API server: listener acquired"
    );
    record_api_listener(
        &api_ready_flag,
        port,
        bound_port,
        acquired.listener.local_addr().ok(),
    );
    offer_api_listener_for_handoff(&acquired.listener, port, bound_port);
    announce_api_ready(&api_ready_flag, &emitter, bound_port);
    await_api_serve(serve_on_dedicated_runtime(
        acquired.listener,
        router,
        bound_port,
    ))
    .await
}

// Adoption is unix-only, so the adoption tests (and what they import) are too.
#[cfg_attr(not(unix), allow(unused_imports))]
#[cfg(test)]
mod listener_adoption_tests {
    use super::{
        acquire_api_listener_with, booting_gate, booting_response, serve_on_dedicated_runtime,
        RouterSlot,
    };
    use qontinui_runner_lib::listeners::{
        adopt_or_bind_from, bind_reuseaddr, AcquiredListener, InheritedListeners, ListenerOrigin,
        API_LISTENER_NAME, ENV_LISTEN_FDNAMES, ENV_LISTEN_FDS, ENV_LISTEN_PID, RUNNER_BOOTING_CODE,
    };
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    /// A store holding one pre-bound listener named `api`, handed over through
    /// the LISTEN_* protocol exactly as systemd would (fork-free: the fd number
    /// is the protocol's start).
    #[cfg(unix)]
    fn store_with_inherited_api() -> (InheritedListeners, SocketAddr) {
        use std::os::fd::IntoRawFd;
        let pre = bind_reuseaddr(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let addr = pre.local_addr().unwrap();
        let fd = pre.into_raw_fd();
        let mut env = HashMap::from([
            (ENV_LISTEN_PID.to_string(), "77".to_string()),
            (ENV_LISTEN_FDS.to_string(), "1".to_string()),
            (
                ENV_LISTEN_FDNAMES.to_string(),
                API_LISTENER_NAME.to_string(),
            ),
        ]);
        let (store, _) = InheritedListeners::from_env(&mut env, 77, fd);
        assert!(env.is_empty(), "LISTEN_* must be cleared after adoption");
        (store, addr)
    }

    /// An adopted listener is used as-is and the fallback loop never moves to
    /// `port + 1` — not even to try binding it.
    #[cfg(unix)]
    #[test]
    fn an_adopted_api_listener_never_falls_back_to_port_plus_one() {
        let (store, addr) = store_with_inherited_api();
        let port = addr.port();
        let binds = Mutex::new(Vec::<u16>::new());
        let (acquired, bound) = acquire_api_listener_with(
            port,
            &[port, port + 1, port + 2],
            store.contains(API_LISTENER_NAME),
            |a| {
                adopt_or_bind_from(&store, API_LISTENER_NAME, a, |b| {
                    binds.lock().unwrap().push(b.port());
                    bind_reuseaddr(b)
                })
            },
        )
        .unwrap();
        assert_eq!(acquired.origin, ListenerOrigin::Inherited);
        assert_eq!(bound, port);
        assert_eq!(acquired.listener.local_addr().unwrap(), addr);
        assert!(binds.lock().unwrap().is_empty(), "nothing may be bound");
    }

    /// With an `api` fd inherited, a failure on the requested port is an
    /// error — never a silent move to `port + 1`, which no client dials.
    #[test]
    fn an_inherited_api_that_fails_is_an_error_not_a_fallback() {
        let tried = Mutex::new(Vec::<u16>::new());
        let err = acquire_api_listener_with(9_876, &[9_876, 9_877, 9_878], true, |a| {
            tried.lock().unwrap().push(a.port());
            Err::<AcquiredListener, _>(std::io::Error::from(std::io::ErrorKind::AddrInUse))
        })
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        assert_eq!(*tried.lock().unwrap(), vec![9_876]);
    }

    /// Control: with nothing inherited the loop still walks the fallback ports
    /// as it always did.
    #[test]
    fn a_plain_launch_keeps_the_fallback_loop() {
        let tried = Mutex::new(Vec::<u16>::new());
        let (acquired, bound) = acquire_api_listener_with(9_876, &[9_876, 9_877], false, |a| {
            tried.lock().unwrap().push(a.port());
            if a.port() == 9_876 {
                Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))
            } else {
                Ok(AcquiredListener {
                    listener: bind_reuseaddr(SocketAddr::from(([127, 0, 0, 1], 0))).unwrap(),
                    origin: ListenerOrigin::Bound,
                })
            }
        })
        .unwrap();
        assert_eq!(bound, 9_877);
        assert_eq!(acquired.origin, ListenerOrigin::Bound);
        assert_eq!(*tried.lock().unwrap(), vec![9_876, 9_877]);
    }

    #[tokio::test]
    async fn the_booting_response_is_a_503_runner_booting_envelope_with_retry_after() {
        let resp = booting_response();
        assert_eq!(resp.status(), 503);
        assert_eq!(resp.headers()["retry-after"], "2");
        let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["success"], false);
        assert_eq!(v["code"], RUNNER_BOOTING_CODE);
    }

    #[cfg(unix)]
    async fn get(client: &reqwest::Client, addr: SocketAddr, path: &str) -> (u16, String, String) {
        let resp = client
            .get(format!("http://{addr}{path}"))
            .send()
            .await
            .expect("a connect during boot must never be refused");
        let status = resp.status().as_u16();
        let retry = resp
            .headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        (status, retry, resp.text().await.unwrap())
    }

    /// The whole boot sequence over real sockets, through the production serve
    /// path (dedicated runtime + peer-user guard): a listener handed over
    /// through LISTEN_* is adopted; a client that connects while NOTHING
    /// accepts yet (the simulated boot delay) gets an answer once serving
    /// starts, not a refusal; requests before the router exists get
    /// `503 RUNNER_BOOTING` + `Retry-After: 2`; once the router is installed the
    /// SAME client (same keep-alive pool) gets `200`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_connect_during_boot_gets_503_then_200_never_a_refusal() {
        let (store, addr) = store_with_inherited_api();
        let listener = store
            .take(API_LISTENER_NAME, addr)
            .unwrap()
            .expect("the inherited api fd is adopted");

        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();

        // Boot delay: the socket exists and nobody accepts. A request made now
        // queues in the backlog.
        let early = tokio::spawn({
            let client = client.clone();
            async move { get(&client, addr, "/livez").await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!early.is_finished(), "nothing accepts yet");

        let slot: RouterSlot = Arc::new(std::sync::OnceLock::new());
        let served = serve_on_dedicated_runtime(listener, booting_gate(slot.clone()), addr.port());

        let (status, retry, body) = early.await.unwrap();
        assert_eq!(status, 503, "{body}");
        assert_eq!(retry, "2");
        assert!(body.contains(RUNNER_BOOTING_CODE), "{body}");

        let (status, _, body) = get(&client, addr, "/anything").await;
        assert_eq!(status, 503);
        assert!(body.contains(RUNNER_BOOTING_CODE), "{body}");

        // The router is ready: install it behind the gate.
        let router = axum::Router::new()
            .route("/livez", axum::routing::get(|| async { "ok" }))
            .fallback(super::not_found_handler);
        assert!(slot.set(router).is_ok());

        let (status, _, body) = get(&client, addr, "/livez").await;
        assert_eq!((status, body.as_str()), (200, "ok"));
        let (status, _, body) = get(&client, addr, "/no/such/route").await;
        assert_eq!(
            status, 404,
            "the real router's own fallback answers: {body}"
        );

        served.abort();
    }
}
