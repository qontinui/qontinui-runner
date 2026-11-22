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
/// than sent `getScreenshot` blind. That NARROWS the window, it does not close
/// it: [`AppDispatcher::dispatch_websocket`] re-reads the row itself, and
/// [`crate::mcp::command_relay::CommandRelay::dispatch`] then routes by app id
/// to whichever connection holds it at send time, so a capability-less client
/// that re-registers in the microseconds between this check and the send can
/// still receive the command — it answers `NO_HANDLER`, which surfaces as
/// `CaptureFailed`. Closing it needs a relay dispatch pinned to the checked
/// `websocket_conn_id`. A registration that turned HTTP is fetched over
/// HTTP. A flip to HTTP between that read and the dispatch
/// ([`DispatchError::NotWebsocket`]) is re-resolved ONCE; a second flip is
/// [`FrameSourceError::Unreachable`]. A row that is gone was live when the
/// capture was routed, so that is `Unreachable` (502, retryable), not an
/// unknown target.
pub(crate) async fn capture_over_relay(
    dispatcher: &AppDispatcher,
    registry: &AppRegistry,
    app_id: &str,
) -> Result<Frame, FrameSourceError> {
    for attempt in 0..2 {
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
                match dispatcher
                    .dispatch_websocket(app_id, SCREENSHOT_ACTION, json!({}))
                    .await
                {
                    Err(DispatchError::NotWebsocket(_)) if attempt == 0 => continue,
                    result => return decode(app_id, result),
                }
            }
        }
    }
    // Reached only if attempt 1 ALSO hit `NotWebsocket` — impossible as the
    // loop is written (the `if attempt == 0` guard sends attempt 1's result to
    // `decode`), but the compiler cannot see that, and if the guard ever
    // changes this is the honest answer: the row flipped transport twice.
    Err(from_dispatch_error(
        app_id,
        DispatchError::NotWebsocket(app_id.to_string()),
    ))
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

/// Map every failure [`AppDispatcher::dispatch_websocket`] can return.
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
        // The HTTP-arm variants: `dispatch_websocket` never takes that arm,
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
