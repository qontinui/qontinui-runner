//! Coord `/ws` subscription for CI dispatches.
//!
//! Design choice (plan Phase 2): a SECOND, dedicated subscription socket
//! with its own `?pattern=` rather than broadening the agent-spawn
//! subscription's glob. Coord's `/ws` is a Redis PSUBSCRIBE bridge (one
//! pattern per connection), so widening the existing pattern would change
//! the agent-runtime connection's URL and delivery set — this keeps the
//! spawn path byte-for-byte unchanged and lets the CI socket exist only
//! while the `ci_node` setting is enabled (checked before every connect, so
//! toggling requires no restart and a disabled fleet runner holds no extra
//! socket).
//!
//! The Redis glob `events.ci.*.<device_id>` matches BOTH
//! `events.ci.build_requested.<device_id>` and
//! `events.ci.build_cancelled.<device_id>` (`*` in Redis patterns crosses
//! word boundaries but the trailing `.<device_id>` still anchors the
//! device), so one pattern covers the whole CI channel family; exact-match
//! routing in [`route_ci_channel`] decides what each frame is.

use std::time::Duration;
use tracing::{debug, info, warn};

use super::{CiCancelPayload, CiDispatchPayload};

/// Base (and reset) reconnect back-off, ms — same posture as
/// `agent_runtime::BACKOFF_BASE_MS`.
const CI_BACKOFF_BASE_MS: u64 = 2_000;
/// Capped maximum reconnect back-off, ms.
const CI_BACKOFF_MAX_MS: u64 = 60_000;
/// A pump that stayed connected at least this long was healthy — reset the
/// back-off on its death (the ALB idle-kill lesson from `agent_runtime`).
const CI_HEALTHY_PUMP_THRESHOLD_SECS: u64 = 30;
/// Unsolicited-Ping cadence keeping the quiet subscription alive through
/// the ALB's ~60s idle reaper.
const CI_KEEPALIVE_INTERVAL_SECS: u64 = 20;
/// Re-check cadence for the `ci_node.enabled` setting while disabled.
const CI_DISABLED_RECHECK_SECS: u64 = 60;

/// Pure builder for the CI-dispatch WS subscription URL. Same
/// normalization rules as `agent_runtime::build_coord_ws_url` (scheme swap,
/// idempotent `/ws` append), but with the CI channel-family pattern.
pub(crate) fn build_ci_ws_url(coord_url: &str, device_id: uuid::Uuid) -> String {
    let base = coord_url.trim().trim_end_matches('/');
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    let ws_base = if ws_base.ends_with("/ws") {
        ws_base
    } else {
        format!("{ws_base}/ws")
    };
    format!("{ws_base}?pattern=events.ci.*.{device_id}")
}

/// Resolve the raw profile `coord_url` (which may already end in `/ws`) —
/// mirrors `agent_runtime::coord_ws_url`'s source of truth.
fn ci_ws_url(device_id: uuid::Uuid) -> Option<String> {
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
    Some(build_ci_ws_url(&coord_url, device_id))
}

/// The two CI channels this device consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CiChannel {
    BuildRequested,
    BuildCancelled,
}

/// Exact-match routing for an inbound frame's channel. `None` for anything
/// that is not this device's CI traffic — including the agent-spawn
/// channels, other devices' CI channels, and future `events.ci.*` kinds.
pub(crate) fn route_ci_channel(channel: &str, device_id: uuid::Uuid) -> Option<CiChannel> {
    if channel == format!("events.ci.build_requested.{device_id}") {
        Some(CiChannel::BuildRequested)
    } else if channel == format!("events.ci.build_cancelled.{device_id}") {
        Some(CiChannel::BuildCancelled)
    } else {
        None
    }
}

/// Extract the inner payload JSON from a coord `/ws` envelope. Coord's
/// fanout emits `{"channel", "payload": "<json-string>"}` (the raw Redis
/// message as a STRING); older/test fixtures use `{"channel", "body":
/// <object>}`. Accept both — same tolerance as
/// `agent_runtime::parse_envelope_payload`.
pub(crate) fn parse_envelope_json(envelope: &serde_json::Value) -> Option<serde_json::Value> {
    let inner = envelope.get("payload").or_else(|| envelope.get("body"))?;
    match inner.as_str() {
        Some(s) => serde_json::from_str(s).ok(),
        None => Some(inner.clone()),
    }
}

/// Subscribe loop: while the `ci_node` setting is enabled, hold a WS
/// subscription for this device's CI channels, reconnecting with capped
/// exponential back-off; while disabled, poll the setting instead of
/// holding a socket.
pub(crate) async fn subscribe_loop(device_id: uuid::Uuid) {
    let mut backoff_ms: u64 = CI_BACKOFF_BASE_MS;
    loop {
        if !crate::settings::get_ci_node_settings().enabled {
            tokio::time::sleep(Duration::from_secs(CI_DISABLED_RECHECK_SECS)).await;
            backoff_ms = CI_BACKOFF_BASE_MS;
            continue;
        }
        let Some(ws_url) = ci_ws_url(device_id) else {
            warn!("ci_node: no coord_url in profile; retrying in {CI_DISABLED_RECHECK_SECS}s");
            tokio::time::sleep(Duration::from_secs(CI_DISABLED_RECHECK_SECS)).await;
            continue;
        };
        let started = std::time::Instant::now();
        let pump_result = connect_and_pump_ci(&ws_url, device_id).await;
        let elapsed = started.elapsed();
        match pump_result {
            Ok(()) => {
                debug!("ci_node: WS pump returned cleanly; reconnecting");
                backoff_ms = CI_BACKOFF_BASE_MS;
            }
            Err(e) => {
                if elapsed.as_secs() >= CI_HEALTHY_PUMP_THRESHOLD_SECS {
                    warn!(
                        "ci_node: WS pump error after {}s of healthy uptime ({e:#}); \
                         resetting back-off",
                        elapsed.as_secs()
                    );
                    backoff_ms = CI_BACKOFF_BASE_MS;
                } else {
                    warn!(
                        "ci_node: WS pump error ({e:#}); retrying in {}s",
                        backoff_ms / 1000
                    );
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        backoff_ms = (backoff_ms * 2).min(CI_BACKOFF_MAX_MS);
    }
}

/// One connect-and-pump iteration — same keepalive/recv discipline as
/// `agent_runtime::connect_and_pump`.
async fn connect_and_pump_ci(ws_url: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use tokio::time::MissedTickBehavior;
    use tokio_tungstenite::tungstenite::Message;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url).await?;
    info!("ci_node: WS connected for device_id={device_id}");

    let mut keepalive = tokio::time::interval(Duration::from_secs(CI_KEEPALIVE_INTERVAL_SECS));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keepalive.tick().await; // skip the immediate first tick

    loop {
        tokio::select! {
            _ = keepalive.tick() => {
                if let Err(e) = ws.send(Message::Ping(Bytes::new())).await {
                    warn!("ci_node: WS keepalive ping send failed: {e:#}");
                    return Err(anyhow::anyhow!("WS keepalive send: {e}"));
                }
            }
            maybe_msg = ws.next() => {
                let msg = match maybe_msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        warn!("ci_node: WS recv error: {e:#}");
                        return Err(anyhow::anyhow!("WS recv: {e}"));
                    }
                    None => return Ok(()),
                };
                let txt = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => {
                        info!("ci_node: WS closed by peer");
                        return Ok(());
                    }
                    Message::Frame(_) => continue,
                };
                if let Err(e) = handle_ci_message(&txt, device_id) {
                    warn!("ci_node: handle_ci_message error (continuing): {e:#}");
                }
            }
        }
    }
}

/// Parse + route one inbound frame. A malformed or foreign frame logs and
/// returns `Ok` — it must never kill the subscribe loop.
fn handle_ci_message(txt: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    let value: serde_json::Value = serde_json::from_str(txt)?;
    let Some(channel) = value.get("channel").and_then(|c| c.as_str()) else {
        // No envelope — not coord's fanout shape; ignore.
        return Ok(());
    };
    let Some(kind) = route_ci_channel(channel, device_id) else {
        return Ok(());
    };
    let Some(inner) = parse_envelope_json(&value) else {
        warn!("ci_node: envelope on {channel} had no parseable payload/body");
        return Ok(());
    };
    match kind {
        CiChannel::BuildRequested => match serde_json::from_value::<CiDispatchPayload>(inner) {
            Ok(payload) => super::admission::submit(payload),
            Err(e) => warn!("ci_node: build_requested payload did not parse: {e}"),
        },
        CiChannel::BuildCancelled => match serde_json::from_value::<CiCancelPayload>(inner) {
            Ok(cancel) => super::admission::cancel(&cancel.dispatch_id),
            Err(e) => warn!("ci_node: build_cancelled payload did not parse: {e}"),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_ws_url_appends_ws_to_bare_host() {
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_ci_ws_url("http://localhost:9870", device),
            format!("ws://localhost:9870/ws?pattern=events.ci.*.{device}")
        );
        assert_eq!(
            build_ci_ws_url("https://coord.qontinui.io", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.ci.*.{device}")
        );
    }

    #[test]
    fn ci_ws_url_does_not_double_append_ws() {
        // The shipped profiles' coord_url already ends in `/ws` — must stay
        // a single `/ws` (a `/ws/ws` 401s at the ALB).
        let device = uuid::Uuid::nil();
        assert_eq!(
            build_ci_ws_url("wss://coord.qontinui.io/ws", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.ci.*.{device}")
        );
        assert_eq!(
            build_ci_ws_url("https://coord.qontinui.io/ws/", device),
            format!("wss://coord.qontinui.io/ws?pattern=events.ci.*.{device}")
        );
    }

    /// Channel routing: the NEW channels route to their arms; the OLD
    /// agent-spawn channels — which arrive on the OTHER socket — must be
    /// `None` here (and conversely, `agent_runtime::handle_message` string-
    /// matches only its own exact channels, so a CI frame is inert there:
    /// the two consumers cannot cross-fire).
    #[test]
    fn route_covers_new_channels_and_ignores_old() {
        let device = uuid::Uuid::now_v7();
        let other = uuid::Uuid::now_v7();
        assert_eq!(
            route_ci_channel(&format!("events.ci.build_requested.{device}"), device),
            Some(CiChannel::BuildRequested)
        );
        assert_eq!(
            route_ci_channel(&format!("events.ci.build_cancelled.{device}"), device),
            Some(CiChannel::BuildCancelled)
        );
        // Another device's CI traffic: not ours.
        assert_eq!(
            route_ci_channel(&format!("events.ci.build_requested.{other}"), device),
            None
        );
        // The old agent-spawn/stop channels: never routed as CI.
        assert_eq!(
            route_ci_channel(&format!("events.agent.spawn_requested.{device}"), device),
            None
        );
        assert_eq!(
            route_ci_channel(&format!("events.agent.stop_requested.{device}"), device),
            None
        );
        // A future CI kind is ignored rather than misrouted.
        assert_eq!(
            route_ci_channel(&format!("events.ci.build_paused.{device}"), device),
            None
        );
    }

    /// Envelope tolerance: payload-as-string (coord's real fanout shape),
    /// body-as-object (legacy/test fixture shape), and garbage.
    #[test]
    fn envelope_payload_and_body_shapes_parse() {
        let inner = serde_json::json!({"dispatch_id": "d-1"});
        let as_string = serde_json::json!({
            "channel": "events.ci.build_cancelled.x",
            "payload": serde_json::to_string(&inner).unwrap(),
        });
        assert_eq!(parse_envelope_json(&as_string), Some(inner.clone()));

        let as_body = serde_json::json!({
            "channel": "events.ci.build_cancelled.x",
            "body": inner,
        });
        assert_eq!(parse_envelope_json(&as_body), Some(inner));

        let junk = serde_json::json!({"channel": "x", "payload": "not json"});
        assert_eq!(parse_envelope_json(&junk), None);

        let no_inner = serde_json::json!({"channel": "x"});
        assert_eq!(parse_envelope_json(&no_inner), None);
    }
}
