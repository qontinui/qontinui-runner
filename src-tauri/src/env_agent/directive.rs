//! Phase 3 — dispatched self-enroll (runner consumer).
//!
//! The runner subscribes to a devenv-enroll directive coord publishes on
//! `events.devenv.enroll_requested.<device_id>`; on receipt it enrolls THIS
//! machine via the shared [`super::enroll`] core and kicks an immediate capture
//! so the drift view populates without waiting for the periodic tick.
//!
//! This is a SECOND, independent coord `/ws` subscription — coord's bridge is
//! one-pattern-per-connection (`coord/src/ws.rs` does a single `PSUBSCRIBE`), so
//! the enroll directive rides its own connection, fully isolated from the
//! critical agent-spawn subscriber in `agent_runtime.rs`. Enroll is a
//! deterministic HTTP call — it needs no agent runtime, so a full
//! `/agents/spawn` coding agent would be overkill (see the plan's OQ#1).
//!
//! The publisher side is coord `POST /devenv/enroll-dispatch` (mirrors
//! `agents_spawn::post_spawn`): the web backend mints the machine + code, then
//! coord validates the target device and publishes the [`EnrollDirective`].

use std::time::Duration;

use serde::Deserialize;
use tracing::{info, warn};

use super::enroll::{self, EnrollParams};

/// The event-channel prefix coord publishes enroll directives on. The full
/// channel is `<PREFIX>.<device_id>`.
const ENROLL_CHANNEL_PREFIX: &str = "events.devenv.enroll_requested";

/// The base (and reset) reconnect back-off.
const BACKOFF_BASE_MS: u64 = 2_000;
/// The capped maximum reconnect back-off.
const BACKOFF_MAX_MS: u64 = 60_000;
/// Keepalive ping cadence — keeps the quiet subscription under coord's ALB
/// ~60s idle timeout (mirrors `agent_runtime`'s Fix (b)).
const KEEPALIVE_INTERVAL_SECS: u64 = 20;

/// Wire shape of the enroll directive coord publishes. The web backend mints the
/// machine + code, then coord publishes THIS payload to the target device's
/// channel. Only `enrollment_code` is required; the rest fall back to the
/// profile-resolved backend + the local `machine.json` identity.
#[derive(Debug, Clone, Deserialize)]
pub struct EnrollDirective {
    /// The one-time enrollment code minted by the web dashboard.
    pub enrollment_code: String,
    /// Web backend base URL. Optional — falls back to the profile-resolved base.
    #[serde(default)]
    pub backend: Option<String>,
    /// Server-minted devenv `machine_id` to enroll AS. Optional — falls back to
    /// the local `machine.json` identity when absent.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// Environment binding echoed from the dispatch (diagnostic; the enroll
    /// response's `environment_id` still wins).
    #[serde(default)]
    pub environment_id: Option<String>,
}

/// Build the device-scoped enroll channel name.
fn enroll_channel(device_id: uuid::Uuid) -> String {
    format!("{ENROLL_CHANNEL_PREFIX}.{device_id}")
}

/// Parse a coord `/ws` envelope (`{channel, payload}`) and, when it is an enroll
/// directive for THIS device, return the parsed directive. Returns `None` for
/// any other frame — coord's default `events.*` subscription delivers foreign
/// frames too, and those must be silently ignored, never error.
pub fn parse_enroll_envelope(txt: &str, device_id: uuid::Uuid) -> Option<EnrollDirective> {
    let value: serde_json::Value = serde_json::from_str(txt).ok()?;
    let channel = value.get("channel").and_then(|c| c.as_str())?;
    if channel != enroll_channel(device_id) {
        return None;
    }
    // Coord wraps the raw Redis message as a STRING in `payload`; tolerate an
    // object payload too (test fixtures / a future non-string bridge).
    match value.get("payload")? {
        serde_json::Value::String(s) => serde_json::from_str(s).ok(),
        other => serde_json::from_value(other.clone()).ok(),
    }
}

/// Enroll THIS machine from a directive via the shared core, then kick a capture
/// so the machine shows up already-reporting. Best-effort: every failure is
/// logged, never panics the subscribe loop.
pub async fn handle_enroll_directive(directive: EnrollDirective) {
    let code = directive.enrollment_code.clone();
    let backend_override = directive.backend.clone();
    let machine_id_override = directive.machine_id.clone();
    let environment_override = directive.environment_id.clone();

    let outcome = tokio::task::spawn_blocking(move || {
        let backend = enroll::resolve_backend_base(backend_override.as_deref())?;
        // Prefer the server-minted machine_id from the directive; fall back to
        // the local machine.json identity (which also supplies the hostname and
        // the coord device_id for the twin bridge).
        let (local_machine_id, hostname, coord_device_id) = enroll::local_machine_identity();
        let machine_id = machine_id_override.or(local_machine_id);
        enroll::run_enroll(EnrollParams {
            code,
            backend,
            machine_id,
            hostname,
            coord_device_id,
            environment_override,
        })
    })
    .await;

    match outcome {
        Ok(Ok(o)) => {
            info!(
                "env_agent::directive: dispatched-enrolled machine_id={} environment_id={} backend={}",
                o.machine_id, o.environment_id, o.backend_url
            );
            // Immediate capture so the drift view populates without waiting for
            // the periodic tick. Best-effort.
            if let Err(e) = super::capture_and_push().await {
                warn!("env_agent::directive: post-enroll capture failed (will retry on tick): {e}");
            }
        }
        Ok(Err(e)) => warn!("env_agent::directive: dispatched enroll failed: {e}"),
        Err(e) => warn!("env_agent::directive: enroll task panicked: {e}"),
    }
}

/// Build the coord `/ws` subscription URL for the enroll directive channel.
/// Mirrors `agent_runtime::build_coord_ws_url` (scheme swap + idempotent `/ws`
/// append) but with the enroll pattern. Pure — unit-tested without profile state.
fn build_enroll_ws_url(coord_url: &str, device_id: uuid::Uuid) -> String {
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
    format!("{ws_base}?pattern={}", enroll_channel(device_id))
}

/// Resolve the coord WS URL from the active profile's `coord_url`. `None` when
/// there's no profile or no `coord_url`.
fn enroll_ws_url(device_id: uuid::Uuid) -> Option<String> {
    let coord_url = crate::profiles::load_strict().ok()?.coord_url?;
    Some(build_enroll_ws_url(&coord_url, device_id))
}

/// This machine's coord device_id (uuid) from `~/.qontinui/machine.json`, reusing
/// the shared identity reader. `None` when absent/unparseable.
fn load_local_device_id() -> Option<uuid::Uuid> {
    let (_machine_id, _hostname, coord_device_id) = enroll::local_machine_identity();
    coord_device_id
}

/// Spawn the enroll-directive subscriber on the ambient tokio runtime. A no-op
/// (logs + returns) when there's no local device identity or no `coord_url`.
/// Wire in `main.rs` next to `spawn_env_capture()`.
pub fn spawn_enroll_directive_subscriber() {
    let device_id = match load_local_device_id() {
        Some(d) => d,
        None => {
            info!(
                "env_agent::directive: no ~/.qontinui/machine.json device_id — \
                 enroll-directive subscriber disabled"
            );
            return;
        }
    };
    let ws_url = match enroll_ws_url(device_id) {
        Some(u) => u,
        None => {
            info!(
                "env_agent::directive: profile has no coord_url — enroll-directive \
                 subscriber disabled"
            );
            return;
        }
    };
    info!("env_agent::directive: starting enroll-directive subscriber for device_id={device_id}");
    tokio::spawn(async move {
        let mut backoff_ms = BACKOFF_BASE_MS;
        loop {
            match connect_and_pump(&ws_url, device_id).await {
                Ok(()) => backoff_ms = BACKOFF_BASE_MS,
                Err(e) => warn!(
                    "env_agent::directive: WS pump error ({e:#}); retrying in {}s",
                    backoff_ms / 1000
                ),
            }
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(BACKOFF_MAX_MS);
        }
    });
}

/// Single connect-and-pump iteration: opens the WS, keepalive-pings, and
/// dispatches each enroll directive. Returns on disconnect.
async fn connect_and_pump(ws_url: &str, device_id: uuid::Uuid) -> anyhow::Result<()> {
    use bytes::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use tokio::time::MissedTickBehavior;
    use tokio_tungstenite::tungstenite::Message;

    let (mut ws, _resp) = tokio_tungstenite::connect_async(ws_url).await?;
    info!("env_agent::directive: WS connected for device_id={device_id}");

    let mut keepalive = tokio::time::interval(Duration::from_secs(KEEPALIVE_INTERVAL_SECS));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Skip the immediate first tick so we don't ping before the socket settles.
    keepalive.tick().await;

    loop {
        tokio::select! {
            _ = keepalive.tick() => {
                if let Err(e) = ws.send(Message::Ping(Bytes::new())).await {
                    return Err(anyhow::anyhow!("WS keepalive send: {e}"));
                }
            }
            maybe_msg = ws.next() => {
                let msg = match maybe_msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => return Err(anyhow::anyhow!("WS recv: {e}")),
                    None => return Ok(()),
                };
                let txt = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                    Message::Close(_) => return Ok(()),
                };
                if let Some(directive) = parse_enroll_envelope(&txt, device_id) {
                    info!(
                        "env_agent::directive: enroll directive received for device_id={device_id}"
                    );
                    handle_enroll_directive(directive).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev() -> uuid::Uuid {
        uuid::Uuid::parse_str("c79a07d5-7e40-49b4-87fa-554c749f9644").unwrap()
    }

    #[test]
    fn build_ws_url_swaps_scheme_and_appends_pattern() {
        let url = build_enroll_ws_url("https://coord.qontinui.io", dev());
        assert_eq!(
            url,
            "wss://coord.qontinui.io/ws?pattern=events.devenv.enroll_requested.c79a07d5-7e40-49b4-87fa-554c749f9644"
        );
        assert!(build_enroll_ws_url("http://localhost:8080", dev())
            .starts_with("ws://localhost:8080/ws?pattern="));
    }

    #[test]
    fn build_ws_url_is_idempotent_on_trailing_ws() {
        // Shipped profiles' coord_url already ends in /ws — don't double-append.
        let url = build_enroll_ws_url("wss://coord.qontinui.io/ws", dev());
        assert!(url.starts_with("wss://coord.qontinui.io/ws?pattern="));
        assert!(!url.contains("/ws/ws"));
    }

    #[test]
    fn parse_accepts_matching_channel_with_string_payload() {
        let envelope = serde_json::json!({
            "channel": format!("events.devenv.enroll_requested.{}", dev()),
            "payload": serde_json::to_string(&serde_json::json!({
                "enrollment_code": "ENR-ABC",
                "backend": "https://qontinui.io",
                "machine_id": "m-123",
                "environment_id": "env-9"
            })).unwrap(),
        })
        .to_string();
        let d = parse_enroll_envelope(&envelope, dev()).expect("should parse");
        assert_eq!(d.enrollment_code, "ENR-ABC");
        assert_eq!(d.backend.as_deref(), Some("https://qontinui.io"));
        assert_eq!(d.machine_id.as_deref(), Some("m-123"));
        assert_eq!(d.environment_id.as_deref(), Some("env-9"));
    }

    #[test]
    fn parse_accepts_object_payload_and_minimal_directive() {
        let envelope = serde_json::json!({
            "channel": format!("events.devenv.enroll_requested.{}", dev()),
            "payload": { "enrollment_code": "ONLY-CODE" },
        })
        .to_string();
        let d = parse_enroll_envelope(&envelope, dev()).expect("should parse");
        assert_eq!(d.enrollment_code, "ONLY-CODE");
        assert!(d.backend.is_none() && d.machine_id.is_none() && d.environment_id.is_none());
    }

    #[test]
    fn parse_rejects_foreign_channel_and_other_devices() {
        // agent-spawn frame for this device → not ours.
        let spawn = serde_json::json!({
            "channel": format!("events.agent.spawn_requested.{}", dev()),
            "payload": "{}",
        })
        .to_string();
        assert!(parse_enroll_envelope(&spawn, dev()).is_none());

        // enroll frame for a DIFFERENT device → not ours.
        let other = uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap();
        let foreign = serde_json::json!({
            "channel": format!("events.devenv.enroll_requested.{}", other),
            "payload": serde_json::to_string(&serde_json::json!({"enrollment_code":"X"})).unwrap(),
        })
        .to_string();
        assert!(parse_enroll_envelope(&foreign, dev()).is_none());
    }

    #[test]
    fn parse_rejects_malformed_frames() {
        assert!(parse_enroll_envelope("not json", dev()).is_none());
        assert!(parse_enroll_envelope("{}", dev()).is_none());
        // matching channel but payload isn't a valid directive (no code).
        let bad = serde_json::json!({
            "channel": format!("events.devenv.enroll_requested.{}", dev()),
            "payload": "{\"backend\":\"https://x\"}",
        })
        .to_string();
        assert!(parse_enroll_envelope(&bad, dev()).is_none());
    }
}
