//! The WebSocket arm of the vision frame resolver: capture a
//! WebSocket-transport app by relaying [`SCREENSHOT_ACTION`] over its own
//! `/ui-bridge/ws` socket, so the CLIENT captures with the handle it owns
//! (Playwright `page.screenshot` in a wrapper, `chrome.tabs.captureVisibleTab`
//! in the extension live relay). Plan
//! `2026-11-21-vision-capture-cannot-frame-a-websocket-transport-app`, D1–D4.
//!
//! The wire contract (D3) is the React Native HTTP handler's: the action is
//! `getScreenshot` and the reply is `{screenshot: <base64 png>, width, height}`
//! (CSS px), decoded by the same [`frame_from_screenshot_json`] the HTTP arm
//! uses. Only an app whose register frame advertises [`SCREENSHOT_CAPABILITY`]
//! is ever sent the command (D2) — see
//! [`super::require_screenshot_capability`].

use std::sync::Arc;

use async_trait::async_trait;
use qontinui_vision_core::Frame;
use serde_json::json;

use super::{
    frame_from_screenshot_json, require_screenshot_capability, DeviceScreenshotSource,
    FrameProvider, FrameSourceError, CODE_APP_RESPONSE_UNREADABLE, CODE_APP_UNREACHABLE,
};
use crate::mcp::app_dispatch::{AppDispatcher, DispatchError};
use crate::mcp::app_registry::{AppRegistry, AppTransport};
use crate::mcp::command_relay::CommandRelayError;
use crate::mcp::types::ApiState;

/// The capability a WebSocket client advertises in its register frame when
/// it answers [`SCREENSHOT_ACTION`]. One constant on each side of the wire.
pub const SCREENSHOT_CAPABILITY: &str = "screenshot";

/// The command action that asks a WebSocket client for one frame — the
/// React Native handler's name (`ui-bridge-native` `types.ts`).
pub const SCREENSHOT_ACTION: &str = "getScreenshot";

/// Captures a WebSocket-transport app over its relay socket.
pub struct WsRelayScreenshotSource {
    pub app_id: String,
}

#[async_trait]
impl FrameProvider for WsRelayScreenshotSource {
    async fn frame(&self, state: &Arc<ApiState>) -> Result<Frame, FrameSourceError> {
        capture_over_relay(&state.app_dispatcher, &state.app_registry, &self.app_id).await
    }

    fn source_label(&self) -> &'static str {
        "websocket_relay"
    }
}

/// Relay [`SCREENSHOT_ACTION`] to `app_id` and decode its reply. Takes the
/// dispatcher and registry rather than an `ApiState` so it is testable
/// against a real served relay (no `tauri::AppHandle` needed).
///
/// The row is READ AGAIN right before every dispatch, and the
/// [`SCREENSHOT_CAPABILITY`] re-checked on it: a capability-less client that
/// re-registered under the same id after resolution is refused (422) rather
/// than sent `getScreenshot` blind. The dispatch is then PINNED to the
/// `websocket_conn_id` that row named
/// ([`AppDispatcher::dispatch_websocket_pinned`]): the command reaches the
/// connection whose capability was just read, or nothing. A client that
/// re-registers between the check and the send — the window the unpinned
/// `dispatch_websocket` left open, where a capability-less replacement
/// received the command and answered `NO_HANDLER` — makes the pinned
/// dispatch refuse with [`CommandRelayError::Displaced`] (the relay checks
/// the routing slot and sends under one lock), and the capture loops back to
/// the re-read, so that replacement is refused 422 `FRAME_SOURCE_UNSUPPORTED`
/// while a capable one is captured on its new connection.
///
/// A registration that turned HTTP is fetched over HTTP. A flip to HTTP
/// between that read and the dispatch ([`DispatchError::NotWebsocket`]) or a
/// replaced connection is re-resolved ONCE; a second one is
/// [`FrameSourceError::Unreachable`] (retryable). A row that is gone was live
/// when the capture was routed, so that is `Unreachable` (502, retryable),
/// not an unknown target.
pub(crate) async fn capture_over_relay(
    dispatcher: &AppDispatcher,
    registry: &AppRegistry,
    app_id: &str,
) -> Result<Frame, FrameSourceError> {
    capture_over_relay_with(dispatcher, registry, app_id, || async {}).await
}

/// How many times [`capture_over_relay`] reads the row: the first read plus
/// ONE re-resolution after a transport flip or a replaced connection.
const CAPTURE_ATTEMPTS: u32 = 2;

/// [`capture_over_relay`], with `after_capability_read` run between the
/// capability check and the dispatch — a no-op in production; the seam lets a
/// test re-register the app at exactly the point the pinning protects.
async fn capture_over_relay_with<F, Fut>(
    dispatcher: &AppDispatcher,
    registry: &AppRegistry,
    app_id: &str,
    mut after_capability_read: F,
) -> Result<Frame, FrameSourceError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for attempt in 0..CAPTURE_ATTEMPTS {
        let entry = registry
            .get_live(app_id)
            .await
            .ok_or_else(|| no_longer_registered(app_id))?;
        match entry.transport {
            AppTransport::Http => {
                return DeviceScreenshotSource::for_app(app_id, &entry)
                    .fetch()
                    .await
            }
            AppTransport::Websocket => {
                require_screenshot_capability(app_id, &entry)?;
                let conn_id = entry.websocket_conn_id.ok_or_else(|| {
                    from_dispatch_error(
                        app_id,
                        DispatchError::WebSocket(CommandRelayError::NotConnected(
                            app_id.to_string(),
                        )),
                    )
                })?;
                after_capability_read().await;
                match dispatcher
                    .dispatch_websocket_pinned(app_id, conn_id, SCREENSHOT_ACTION, json!({}))
                    .await
                {
                    Err(e) if is_rerouted(&e) && attempt + 1 < CAPTURE_ATTEMPTS => continue,
                    result => return decode(app_id, result),
                }
            }
        }
    }
    // Reached only if the LAST attempt was rerouted too — impossible as the
    // loop is written (the `attempt + 1 < CAPTURE_ATTEMPTS` guard sends the
    // last attempt's result to `decode`), but the compiler cannot see that,
    // and if the guard ever changes this is the honest answer: the row moved
    // on every attempt.
    Err(from_dispatch_error(
        app_id,
        DispatchError::NotWebsocket(app_id.to_string()),
    ))
}

/// A dispatch refused because the app's registration moved after it was
/// read — to HTTP, or to another connection. Both are re-resolved from a
/// fresh row rather than reported.
fn is_rerouted(err: &DispatchError) -> bool {
    matches!(
        err,
        DispatchError::NotWebsocket(_) | DispatchError::WebSocket(CommandRelayError::Displaced(_))
    )
}

/// The app's row vanished after the capture was routed to it.
fn no_longer_registered(app_id: &str) -> FrameSourceError {
    FrameSourceError::Unreachable {
        message: format!(
            "app '{app_id}' is no longer registered (it was live when its capture was routed)"
        ),
        code: CODE_APP_UNREACHABLE,
        retryable: Some(true),
    }
}

/// Decode a relay reply into a frame, or map the relay's failure.
fn decode(
    app_id: &str,
    result: Result<serde_json::Value, DispatchError>,
) -> Result<Frame, FrameSourceError> {
    let body = result.map_err(|e| from_dispatch_error(app_id, e))?;
    frame_from_screenshot_json(&body).map_err(|e| FrameSourceError::CaptureFailed {
        message: format!("app '{app_id}' over /ui-bridge/ws: {e}"),
        code: None,
        retryable: None,
    })
}

/// Map every failure [`AppDispatcher::dispatch_websocket_pinned`] can return.
pub(crate) fn from_dispatch_error(app_id: &str, err: DispatchError) -> FrameSourceError {
    let unreachable = |message: String, retryable: Option<bool>| FrameSourceError::Unreachable {
        message,
        code: CODE_APP_UNREACHABLE,
        retryable,
    };
    match err {
        // The target was live at resolve time (only a live row is routed
        // here), so its disappearance is the transport's, not an unknown id.
        DispatchError::NotRegistered(id) => no_longer_registered(&id),
        DispatchError::NotWebsocket(id) => unreachable(
            format!(
                "app '{id}' changed transport while its capture was being routed, \
                 and again after it was re-resolved"
            ),
            Some(true),
        ),
        DispatchError::WebSocket(relay) => match relay {
            CommandRelayError::WrapperError(failure) => FrameSourceError::CaptureFailed {
                message: format!("app '{app_id}' failed to capture a screenshot: {failure}"),
                code: failure.code,
                retryable: failure.retryable,
            },
            CommandRelayError::UnreadableResponse(detail) => FrameSourceError::Unreachable {
                message: format!(
                    "app '{app_id}' answered {SCREENSHOT_ACTION}, but its response frame \
                     could not be read: {detail}"
                ),
                code: CODE_APP_RESPONSE_UNREADABLE,
                retryable: None,
            },
            CommandRelayError::Timeout(after) => unreachable(
                format!(
                    "app '{app_id}' did not answer {SCREENSHOT_ACTION} within {}ms \
                     (the relay's command timeout)",
                    after.as_millis()
                ),
                None,
            ),
            e @ (CommandRelayError::NotConnected(_)
            | CommandRelayError::Disconnected
            | CommandRelayError::Displaced(_)) => unreachable(
                format!("app '{app_id}' over /ui-bridge/ws: {e}"),
                Some(true),
            ),
            e @ CommandRelayError::EncodeFailed(_) => {
                FrameSourceError::RunnerCapture(format!("app '{app_id}': {e}"))
            }
        },
        // The HTTP-arm variants: the WebSocket dispatch never takes that arm,
        // but every variant is mapped rather than defaulted.
        DispatchError::AppAnswered {
            error,
            code,
            retryable,
            ..
        } => FrameSourceError::CaptureFailed {
            message: format!("app '{app_id}' failed to capture a screenshot: {error}"),
            code,
            retryable,
        },
        e @ DispatchError::InvalidJson(_) => FrameSourceError::Unreachable {
            message: format!("app '{app_id}': {e}"),
            code: CODE_APP_RESPONSE_UNREADABLE,
            retryable: None,
        },
        e @ (DispatchError::NotConnected
        | DispatchError::NotResponsive(_)
        | DispatchError::HttpSend { .. }
        | DispatchError::HttpStatus { .. }) => unreachable(format!("app '{app_id}': {e}"), None),
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::png_b64;
    use super::super::{CODE_APP_CAPTURE_FAILED, CODE_RUNNER_CAPTURE_FAILED};
    use super::*;
    use crate::mcp::app_dispatch::test_support::{next_frame, sample_app};
    use crate::mcp::command_relay::{CommandRelay, CommandResponse, WrapperFailure};
    use crate::mcp::relay_binding::BindingMode;
    use crate::mcp::ws_relay::WsConnectionManager;
    use axum::http::StatusCode;
    use std::time::Duration;

    /// A WebSocket-registered app `app_id` advertising `capabilities`, on an
    /// in-memory relay with `timeout`. Returns the dispatcher, the registry,
    /// the relay, the app's conn id and its outbound frame channel.
    async fn ws_app(
        app_id: &str,
        capabilities: &[&str],
        timeout: Duration,
    ) -> (
        Arc<AppDispatcher>,
        Arc<AppRegistry>,
        Arc<CommandRelay>,
        u64,
        tokio::sync::mpsc::Receiver<String>,
    ) {
        let registry = AppRegistry::new();
        let ws = WsConnectionManager::new();
        let (conn_id, outbound_rx) = ws.test_register(app_id).await;
        let mut app = sample_app(app_id, "ws://runner/ui-bridge/ws");
        app.capabilities = capabilities.iter().map(|c| c.to_string()).collect();
        registry
            .upsert(app, None, AppTransport::Websocket, Some(conn_id), None)
            .await;
        let relay = CommandRelay::with_timeout(ws, timeout);
        let dispatcher = AppDispatcher::new(registry.clone(), relay.clone());
        (dispatcher, registry, relay, conn_id, outbound_rx)
    }

    /// The `commandId` of the next frame, asserting it asks for a screenshot.
    async fn screenshot_command(outbound_rx: &mut tokio::sync::mpsc::Receiver<String>) -> String {
        let frame: serde_json::Value =
            serde_json::from_str(&next_frame(outbound_rx).await).unwrap();
        assert_eq!(frame["action"], SCREENSHOT_ACTION, "{frame}");
        frame["commandId"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn a_capable_client_reply_decodes_into_a_frame() {
        let (dispatcher, registry, relay, conn_id, mut rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_secs(5)).await;
        let pending =
            tokio::spawn(async move { capture_over_relay(&dispatcher, &registry, "wapp").await });
        let command_id = screenshot_command(&mut rx).await;
        relay
            .resolve(
                conn_id,
                BindingMode::Enforce,
                CommandResponse {
                    command_id,
                    success: true,
                    result: Some(json!({"screenshot": png_b64(80, 40), "width": 40, "height": 20})),
                    error: None,
                },
            )
            .await;
        let frame = pending.await.unwrap().expect("frame");
        assert_eq!((frame.width, frame.height), (80, 40));
        assert!((frame.source.scale_factor - 2.0).abs() < 1e-9);
    }

    /// (d) A client that answers `success:false` with a code maps to
    /// `CaptureFailed` carrying that code and its retry verdict.
    #[tokio::test]
    async fn a_client_failure_with_a_code_maps_to_capture_failed_carrying_it() {
        let (dispatcher, registry, relay, conn_id, mut rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_secs(5)).await;
        let pending =
            tokio::spawn(async move { capture_over_relay(&dispatcher, &registry, "wapp").await });
        let command_id = screenshot_command(&mut rx).await;
        relay
            .resolve(
                conn_id,
                BindingMode::Enforce,
                CommandResponse {
                    command_id,
                    success: false,
                    result: None,
                    error: Some(WrapperFailure {
                        code: Some("TAB_NOT_VISIBLE".into()),
                        message: "the tab is hidden".into(),
                        retryable: Some(true),
                    }),
                },
            )
            .await;
        let err = pending.await.unwrap().expect_err("must fail");
        assert!(
            matches!(
                &err,
                FrameSourceError::CaptureFailed { code: Some(c), retryable: Some(true), .. }
                    if c == "TAB_NOT_VISIBLE"
            ),
            "{err:?}"
        );
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(err.code(), "TAB_NOT_VISIBLE");
        assert!(err.to_string().contains("the tab is hidden"), "{err}");
        let (status, body) = err.to_json_err();
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body.0.code.as_deref(), Some("TAB_NOT_VISIBLE"));
        let hint = body.0.hint.clone().expect("hint");
        assert_eq!(hint["failure_origin"], "app");
        assert_eq!(hint["retryable"], true);
    }

    /// (e) A client that never answers maps to `Unreachable`, naming the timeout.
    #[tokio::test]
    async fn a_relay_timeout_maps_to_unreachable_naming_the_timeout() {
        let (dispatcher, registry, _relay, _conn_id, mut rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_millis(150)).await;
        let pending =
            tokio::spawn(async move { capture_over_relay(&dispatcher, &registry, "wapp").await });
        let _ = screenshot_command(&mut rx).await;
        let err = tokio::time::timeout(Duration::from_secs(5), pending)
            .await
            .expect("settles at the relay timeout")
            .unwrap()
            .expect_err("must fail");
        assert!(
            matches!(&err, FrameSourceError::Unreachable { code, retryable: None, .. }
                if *code == CODE_APP_UNREACHABLE),
            "{err:?}"
        );
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
        assert!(err.to_string().contains("150ms"), "{err}");
        let (_, body) = err.to_json_err();
        assert_eq!(
            body.0.hint.clone().expect("hint")["failure_origin"],
            "transport"
        );
    }

    /// A reply with no `screenshot` is the app's failed capture, not a
    /// transport fault; an unreadable response frame is the transport's.
    #[test]
    fn every_relay_failure_maps_to_its_class() {
        let missing = decode("wapp", Ok(json!({"width": 10}))).expect_err("no screenshot");
        assert_eq!(missing.code(), CODE_APP_CAPTURE_FAILED);

        let cases = [
            (
                DispatchError::NotRegistered("wapp".into()),
                StatusCode::BAD_GATEWAY,
                CODE_APP_UNREACHABLE,
            ),
            (
                DispatchError::NotWebsocket("wapp".into()),
                StatusCode::BAD_GATEWAY,
                CODE_APP_UNREACHABLE,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::NotConnected("wapp".into())),
                StatusCode::BAD_GATEWAY,
                CODE_APP_UNREACHABLE,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::Disconnected),
                StatusCode::BAD_GATEWAY,
                CODE_APP_UNREACHABLE,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::Displaced("new tab".into())),
                StatusCode::BAD_GATEWAY,
                CODE_APP_UNREACHABLE,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::UnreadableResponse("bad".into())),
                StatusCode::BAD_GATEWAY,
                CODE_APP_RESPONSE_UNREADABLE,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::WrapperError(
                    WrapperFailure::message_only("boom"),
                )),
                StatusCode::BAD_GATEWAY,
                CODE_APP_CAPTURE_FAILED,
            ),
            (
                DispatchError::WebSocket(CommandRelayError::EncodeFailed(
                    serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
                )),
                StatusCode::INTERNAL_SERVER_ERROR,
                CODE_RUNNER_CAPTURE_FAILED,
            ),
        ];
        for (err, status, code) in cases {
            let label = format!("{err:?}");
            let mapped = from_dispatch_error("wapp", err);
            assert_eq!(mapped.status(), status, "{label} → {mapped:?}");
            assert_eq!(mapped.code(), code, "{label} → {mapped:?}");
        }
    }

    /// Pins the CAPTURE-TIME capability re-check: a row that carries no
    /// `screenshot` capability when the capture runs (a client that
    /// re-registered under the same id after resolution) is refused (422) and
    /// sent no frame, independently of the resolver's own check.
    #[tokio::test]
    async fn the_capture_time_capability_recheck_refuses_a_capability_less_row() {
        let (dispatcher, registry, _relay, _conn_id, mut rx) =
            ws_app("wapp", &["control"], Duration::from_secs(5)).await;
        let err = capture_over_relay(&dispatcher, &registry, "wapp")
            .await
            .expect_err("must refuse");
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "{err:?}");
        assert_eq!(err.code(), "FRAME_SOURCE_UNSUPPORTED");
        assert!(rx.try_recv().is_err(), "no getScreenshot may be sent blind");
    }

    /// Re-register `app_id` on a FRESH connection advertising `capabilities`
    /// — the wrapper reconnecting. Returns the new conn id and its outbound
    /// channel.
    async fn reconnect(
        relay: &CommandRelay,
        registry: &AppRegistry,
        app_id: &str,
        capabilities: &[&str],
    ) -> (u64, tokio::sync::mpsc::Receiver<String>) {
        let (conn_id, rx) = relay.ws_manager().test_register(app_id).await;
        let mut app = sample_app(app_id, "ws://runner/ui-bridge/ws");
        app.capabilities = capabilities.iter().map(|c| c.to_string()).collect();
        registry
            .upsert(app, None, AppTransport::Websocket, Some(conn_id), None)
            .await;
        (conn_id, rx)
    }

    /// A screenshot reply for `command_id`, resolved on `conn_id`.
    async fn answer_screenshot(relay: &CommandRelay, conn_id: u64, command_id: String) {
        relay
            .resolve(
                conn_id,
                BindingMode::Enforce,
                CommandResponse {
                    command_id,
                    success: true,
                    result: Some(json!({"screenshot": png_b64(80, 40), "width": 80, "height": 40})),
                    error: None,
                },
            )
            .await;
    }

    type Replacement = Arc<tokio::sync::Mutex<Option<(u64, tokio::sync::mpsc::Receiver<String>)>>>;

    /// A hook that reconnects `app_id` advertising `capabilities` the FIRST
    /// time it runs — between the capability read and the send — and parks
    /// the new connection in the returned slot.
    fn reconnect_once(
        relay: &Arc<CommandRelay>,
        registry: &Arc<AppRegistry>,
        app_id: &'static str,
        capabilities: &'static [&'static str],
    ) -> (
        Replacement,
        impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
    ) {
        let slot: Replacement = Arc::new(tokio::sync::Mutex::new(None));
        let (relay, registry, hook_slot) = (relay.clone(), registry.clone(), slot.clone());
        let mut reconnected = false;
        let hook = move || {
            let first = !reconnected;
            reconnected = true;
            let (relay, registry, slot) = (relay.clone(), registry.clone(), hook_slot.clone());
            Box::pin(async move {
                if first {
                    *slot.lock().await =
                        Some(reconnect(&relay, &registry, app_id, capabilities).await);
                }
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        };
        (slot, hook)
    }

    /// Phase 2 (plan `2026-11-22-vision-capture-websocket-follow-ups`): the
    /// capability is read on one connection, and a capability-LESS client
    /// reconnects under the same id before the send. The capture is refused
    /// 422 FRAME_SOURCE_UNSUPPORTED, and NOTHING reaches either connection —
    /// before the pin, the command was routed to the replacement, which
    /// answered `NO_HANDLER`.
    #[tokio::test]
    async fn a_capability_less_reconnect_after_the_capability_read_is_refused_422() {
        let (dispatcher, registry, relay, _old_conn, mut old_rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_secs(5)).await;
        let (slot, hook) = reconnect_once(&relay, &registry, "wapp", &["control"]);
        let err = tokio::time::timeout(
            Duration::from_secs(10),
            capture_over_relay_with(&dispatcher, &registry, "wapp", hook),
        )
        .await
        .expect("the capture settles without waiting on a reply")
        .expect_err("a capability-less replacement must be refused");
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "{err:?}");
        assert_eq!(err.code(), "FRAME_SOURCE_UNSUPPORTED");
        let (_new_conn, mut new_rx) = slot.lock().await.take().expect("reconnected");
        assert!(
            new_rx.try_recv().is_err(),
            "getScreenshot reached a connection that never advertised screenshot"
        );
        assert!(
            old_rx.try_recv().is_err(),
            "the replaced connection got a frame"
        );
        assert_eq!(relay.pending_count().await, 0);
    }

    /// The same connection between the capability read and the send (its
    /// row merely refreshed) is captured: the pin does not refuse the
    /// connection it was taken from.
    #[tokio::test]
    async fn the_same_connection_after_the_capability_read_is_captured() {
        let (dispatcher, registry, relay, conn_id, mut rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_secs(5)).await;
        let hook_registry = registry.clone();
        let pending = tokio::spawn(async move {
            capture_over_relay_with(&dispatcher, &registry, "wapp", || {
                let registry = hook_registry.clone();
                async move {
                    let mut app = sample_app("wapp", "ws://runner/ui-bridge/ws");
                    app.capabilities = vec![SCREENSHOT_CAPABILITY.to_string()];
                    registry
                        .upsert(app, None, AppTransport::Websocket, Some(conn_id), None)
                        .await;
                }
            })
            .await
        });
        let command_id = screenshot_command(&mut rx).await;
        answer_screenshot(&relay, conn_id, command_id).await;
        let frame = pending.await.unwrap().expect("frame");
        assert_eq!((frame.width, frame.height), (80, 40));
    }

    /// A CAPABLE client that reconnects between the read and the send is
    /// re-resolved and captured on its NEW connection — never on the old one.
    #[tokio::test]
    async fn a_capable_reconnect_after_the_capability_read_is_captured_on_the_new_connection() {
        let (dispatcher, registry, relay, _old_conn, mut old_rx) =
            ws_app("wapp", &[SCREENSHOT_CAPABILITY], Duration::from_secs(5)).await;
        let (slot, hook) = reconnect_once(&relay, &registry, "wapp", &[SCREENSHOT_CAPABILITY]);
        let pending = tokio::spawn(async move {
            capture_over_relay_with(&dispatcher, &registry, "wapp", hook).await
        });
        let (new_conn, mut new_rx) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(r) = slot.lock().await.take() {
                    break r;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the hook reconnected");
        let command_id = screenshot_command(&mut new_rx).await;
        answer_screenshot(&relay, new_conn, command_id).await;
        pending
            .await
            .unwrap()
            .expect("frame from the new connection");
        assert!(
            old_rx.try_recv().is_err(),
            "the replaced connection got a frame"
        );
    }

    /// A4: a row that vanished after routing is 502 APP_UNREACHABLE, retryable.
    #[tokio::test]
    async fn a_row_gone_after_routing_is_unreachable_not_unknown() {
        let registry = AppRegistry::new();
        let relay = CommandRelay::with_timeout(WsConnectionManager::new(), Duration::from_secs(1));
        let dispatcher = AppDispatcher::new(registry.clone(), relay);
        let err = capture_over_relay(&dispatcher, &registry, "gone")
            .await
            .expect_err("must fail");
        assert!(
            matches!(&err, FrameSourceError::Unreachable { code, retryable: Some(true), .. }
                if *code == CODE_APP_UNREACHABLE),
            "{err:?}"
        );
        assert_eq!(err.status(), StatusCode::BAD_GATEWAY);
    }
}
