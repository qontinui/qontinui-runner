//! The ONE coord `/ws` upgrade for every runner subscriber lane.
//!
//! Four lanes hold a socket on coord's `/ws` Redis pub/sub bridge: the
//! agent-spawn subscriber (`agent_runtime`), the CI-dispatch subscriber
//! (`ci_node::subscription`), the devenv-directive subscriber
//! (`env_agent::directive`) and the session-handoff receiver
//! (`session::handoff`). Each used to carry its own copy of the URL
//! normalization and connect with a bare `?pattern=<redis glob>` and no
//! credential. Plan
//! `2026-09-13-coord-publishes-agent-jwts-on-a-redis-channel-fronted-by-an-unauthenticated-ws-firehose`
//! Phase 2 closed that: coord now requires a device JWT at the upgrade and
//! replaced the caller-supplied glob with a CLOSED set of named
//! subscriptions, each mapped server-side to the pattern the TOKEN's
//! principal is entitled to. `?pattern=` is refused 400 by that coord.
//!
//! This module is the single place that knows both halves:
//!
//! - [`build_ws_url`] — scheme swap, idempotent `/ws`, `?subscribe=<name>`;
//! - [`connect`] — resolve the device JWT PER CONNECT and present it as
//!   `?token=` AND `Authorization: Bearer`, then name any refusal
//!   (status + coord's `error` code) so a 401/403 reconnect flap is
//!   diagnosable from the log.
//!
//! It lives in the lib crate because `env_agent` does, and the bin crate's
//! lanes reach it as `qontinui_runner_lib::coord_ws`. The bearer accessor it
//! calls is `auth::device_bearer_scoped` — the same accessor behind
//! `auth::attach_device_auth`, reading the on-disk credential slot per call,
//! so a reconnect after the ~4 h device-JWT rollover presents the fresh token.

use tokio_tungstenite::tungstenite;
use tracing::warn;

/// The closed set of subscriptions coord's `/ws` admits, by wire name.
///
/// Every variant is resolved SERVER-SIDE from the verified token's claims —
/// the device_id (and, for [`Subscription::Sessions`], the tenant_id) is the
/// token's own, never a caller-supplied uuid, which is why no builder here
/// takes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscription {
    /// `events.agent.spawn_requested.<device>` and
    /// `events.agent.stop_requested.<device>` — the agent-spawn lane.
    Device,
    /// `events.ci.*.<device>` — the CI-dispatch lane (`build_requested`,
    /// `build_cancelled`, `settings_requested`).
    DeviceCi,
    /// `events.devenv.*.<device>` — the devenv-directive lane
    /// (`enroll_requested`, `repos_apply_requested`).
    DeviceDevenv,
    /// `qontinui.sessions.<tenant>.<device>.*` — the session-handoff lane.
    /// TIGHTER than the `qontinui.sessions.*` glob it replaces: only this
    /// device's subjects under the token's tenant arrive.
    Sessions,
}

impl Subscription {
    /// The `?subscribe=` value coord recognises.
    pub const fn name(self) -> &'static str {
        match self {
            Subscription::Device => "device",
            Subscription::DeviceCi => "device_ci",
            Subscription::DeviceDevenv => "device_devenv",
            Subscription::Sessions => "sessions",
        }
    }
}

/// Pure builder for a coord `/ws` subscription URL.
///
/// Normalization rule:
/// 1. Trim whitespace and any trailing `/`.
/// 2. Swap the scheme: `https://`→`wss://`, `http://`→`ws://`; leave an
///    already-`ws(s)://` base (or any other scheme) untouched.
/// 3. Append `/ws` ONLY if the base does not already end in `/ws` (the
///    trailing `/` was stripped in step 1, so a `…/ws/` input is handled
///    too). Idempotent for the shipped profiles whose `coord_url` already
///    ends in `/ws`, while still appending it for a bare host URL.
/// 4. Append `?subscribe=<name>`.
///
/// The credential is NOT part of the URL this returns: [`connect`] attaches
/// it per connect, so the URL a lane logs or holds across reconnects never
/// carries a token.
pub fn build_ws_url(coord_url: &str, subscription: Subscription) -> String {
    let base = coord_url.trim().trim_end_matches('/');
    let ws_base = base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .or_else(|| {
            base.strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
        })
        .unwrap_or_else(|| base.to_string());
    // Idempotent: don't double-append `/ws` when the base already ends in it.
    let ws_base = if ws_base.ends_with("/ws") {
        ws_base
    } else {
        format!("{ws_base}/ws")
    };
    format!("{ws_base}?subscribe={}", subscription.name())
}

/// Append `token=<jwt>` to a subscription URL — `&` when the URL already
/// carries a query (it always does: [`build_ws_url`] emits `?subscribe=`),
/// `?` otherwise. A JWT is base64url segments joined by `.`, all URL-safe,
/// so no percent-encoding is needed.
pub fn ws_url_with_token(ws_url: &str, token: &str) -> String {
    let sep = if ws_url.contains('?') { '&' } else { '?' };
    format!("{ws_url}{sep}token={token}")
}

/// A live coord `/ws` socket.
pub type CoordWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open a coord `/ws` subscription, presenting the device JWT BOTH as
/// `?token=` (the carriage coord's in-handler check offers browser clients,
/// which cannot set a header on an upgrade) and as `Authorization: Bearer`
/// (the runner is not a browser; coord reads the header first).
///
/// The token is resolved HERE, per call — every lane's reconnect loop calls
/// this afresh, so the socket that follows a ~4 h device-JWT rollover carries
/// the new token, never one captured at startup. Coord checks it at upgrade
/// only, exactly like `/ws/device-status`.
///
/// No credential available → connect anonymously and WARN naming the reason:
/// a coord predating the authenticated `/ws` admits the socket; a coord
/// requiring one answers 401, which [`log_upgrade_failure`] names so the
/// resulting reconnect flap is diagnosable. The token is never logged, and
/// neither is the request URL (which carries it).
///
/// `lane` is the log prefix of the calling subscriber.
pub async fn connect(ws_url: &str, lane: &str) -> anyhow::Result<CoordWs> {
    let request = build_upgrade_request(ws_url, lane)?;
    match tokio_tungstenite::connect_async(request).await {
        Ok((ws, _resp)) => Ok(ws),
        Err(e) => {
            log_upgrade_failure(lane, &e);
            Err(anyhow::anyhow!("WS upgrade: {e}"))
        }
    }
}

/// Build the upgrade request [`connect`] sends. Split out so the credential
/// carriage (query param + header, or neither with a warning) is inspectable
/// without a socket.
pub(crate) fn build_upgrade_request(
    ws_url: &str,
    lane: &str,
) -> anyhow::Result<tungstenite::handshake::client::Request> {
    let token = crate::auth::device_bearer_scoped(crate::auth::TenantScope::Device);
    if token.is_none() {
        warn!(
            "{lane}: no device JWT available for the coord /ws upgrade (unpaired runner, \
             empty credential slot, or unreadable secure storage) — connecting WITHOUT a \
             credential; a coord that requires one will refuse the upgrade 401"
        );
    }
    upgrade_request_with(ws_url, token.as_deref())
}

/// Pure core of [`build_upgrade_request`]: the request for `ws_url` carrying
/// `token` on both carriages, or the bare request when there is none.
fn upgrade_request_with(
    ws_url: &str,
    token: Option<&str>,
) -> anyhow::Result<tungstenite::handshake::client::Request> {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::header::{HeaderValue, AUTHORIZATION};

    let Some(token) = token else {
        return ws_url
            .into_client_request()
            .map_err(|e| anyhow::anyhow!("WS upgrade request: {e}"));
    };
    let mut request = ws_url_with_token(ws_url, token)
        .into_client_request()
        .map_err(|e| anyhow::anyhow!("WS upgrade request: {e}"))?;
    let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|e| anyhow::anyhow!("WS upgrade Authorization header: {e}"))?;
    request.headers_mut().insert(AUTHORIZATION, bearer);
    Ok(request)
}

/// Name an upgrade refusal so a 401/403 flap is diagnosable from the log: the
/// HTTP status plus the `error` field of coord's JSON refusal body (coord's
/// refusals are `{"error": "<code>", …}`). Only the code is logged, never the
/// request — the URL carries the token.
pub fn log_upgrade_failure(lane: &str, e: &tungstenite::Error) {
    match e {
        tungstenite::Error::Http(resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body()
                .as_deref()
                .map(|b| String::from_utf8_lossy(b).into_owned())
                .unwrap_or_default();
            let hint = match status {
                400 => {
                    " — coord refused the request shape; a `?pattern=` query is no longer \
                        accepted (this build sends `?subscribe=`, so check for a proxy rewrite)"
                }
                401 => {
                    " — coord requires a device JWT on the upgrade and none was accepted \
                        (unpaired runner, expired credential, or revoked device)"
                }
                403 => {
                    " — the credential was accepted but this subscription is not admitted \
                        for its principal"
                }
                _ => "",
            };
            warn!(
                "{lane}: coord /ws upgrade refused status={status} error={}{hint}",
                refusal_error_code(&body).as_deref().unwrap_or("<none>"),
            );
        }
        other => warn!("{lane}: coord /ws upgrade failed before any response: {other}"),
    }
}

/// The `error` field of a coord refusal body, when the body is a JSON object
/// carrying one. `None` for an empty body, a non-JSON body (an ALB or proxy
/// page), or a JSON body with no string `error`.
pub fn refusal_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_ws_url_appends_ws_to_bare_host_and_swaps_scheme() {
        assert_eq!(
            build_ws_url("http://localhost:9870", Subscription::Device),
            "ws://localhost:9870/ws?subscribe=device"
        );
        assert_eq!(
            build_ws_url("https://coord.qontinui.io", Subscription::DeviceCi),
            "wss://coord.qontinui.io/ws?subscribe=device_ci"
        );
    }

    #[test]
    fn build_ws_url_does_not_double_append_ws() {
        // The shipped `dev`/`production` profiles' coord_url ALREADY ends in
        // `/ws` (see bin/qontinui_profile.rs). Must produce a single `/ws`,
        // not `/ws/ws` (which 401s at the ALB and blocks the subscribe loop).
        assert_eq!(
            build_ws_url("wss://coord.qontinui.io/ws", Subscription::Device),
            "wss://coord.qontinui.io/ws?subscribe=device"
        );
        assert_eq!(
            build_ws_url("ws://localhost:9870/ws", Subscription::DeviceDevenv),
            "ws://localhost:9870/ws?subscribe=device_devenv"
        );
        // https→wss conversion preserved on an already-`/ws` https base.
        assert_eq!(
            build_ws_url("https://coord.qontinui.io/ws", Subscription::Sessions),
            "wss://coord.qontinui.io/ws?subscribe=sessions"
        );
    }

    #[test]
    fn build_ws_url_normalizes_trailing_slash_and_whitespace() {
        assert_eq!(
            build_ws_url("wss://coord.qontinui.io/ws/", Subscription::Device),
            "wss://coord.qontinui.io/ws?subscribe=device"
        );
        assert_eq!(
            build_ws_url("https://coord.qontinui.io/", Subscription::Device),
            "wss://coord.qontinui.io/ws?subscribe=device"
        );
        assert_eq!(
            build_ws_url("  https://coord.qontinui.io  ", Subscription::Device),
            "wss://coord.qontinui.io/ws?subscribe=device"
        );
    }

    #[test]
    fn build_ws_url_preserves_already_ws_scheme() {
        assert_eq!(
            build_ws_url("ws://h:9870/ws", Subscription::Device),
            "ws://h:9870/ws?subscribe=device"
        );
    }

    /// The URL never carries a `?pattern=` — the new coord refuses it 400 and
    /// the old one would PSUBSCRIBE a caller-chosen glob, which is the leak.
    #[test]
    fn build_ws_url_never_emits_a_pattern_query() {
        for sub in [
            Subscription::Device,
            Subscription::DeviceCi,
            Subscription::DeviceDevenv,
            Subscription::Sessions,
        ] {
            let url = build_ws_url("https://coord.qontinui.io", sub);
            assert!(!url.contains("pattern="), "{url}");
            assert!(
                url.ends_with(&format!("?subscribe={}", sub.name())),
                "{url}"
            );
        }
    }

    #[test]
    fn subscription_names_are_the_closed_set_coord_admits() {
        assert_eq!(Subscription::Device.name(), "device");
        assert_eq!(Subscription::DeviceCi.name(), "device_ci");
        assert_eq!(Subscription::DeviceDevenv.name(), "device_devenv");
        assert_eq!(Subscription::Sessions.name(), "sessions");
    }

    #[test]
    fn ws_url_with_token_uses_ampersand_after_an_existing_query() {
        assert_eq!(
            ws_url_with_token("wss://c/ws?subscribe=device", "a.b.c"),
            "wss://c/ws?subscribe=device&token=a.b.c"
        );
        assert_eq!(
            ws_url_with_token("wss://c/ws", "a.b.c"),
            "wss://c/ws?token=a.b.c"
        );
    }

    /// Both carriages: the token rides the query string (what a browser
    /// could send) AND the `Authorization` header (what coord reads first).
    #[test]
    fn upgrade_request_carries_the_token_as_query_and_bearer_header() {
        let req =
            upgrade_request_with("wss://coord.example/ws?subscribe=device", Some("t.o.k")).unwrap();
        assert_eq!(req.uri().query(), Some("subscribe=device&token=t.o.k"));
        assert_eq!(
            req.headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok()),
            Some("Bearer t.o.k")
        );
    }

    /// No credential → a bare request, so an old coord still admits the
    /// socket (and a new one refuses 401 with a named reason).
    #[test]
    fn upgrade_request_without_a_token_is_bare() {
        let req = upgrade_request_with("wss://coord.example/ws?subscribe=device", None).unwrap();
        assert_eq!(req.uri().query(), Some("subscribe=device"));
        assert!(req.headers().get("authorization").is_none());
    }

    #[test]
    fn refusal_error_code_reads_only_a_string_error_field() {
        assert_eq!(
            refusal_error_code(r#"{"error":"pattern_removed","hint":"use ?subscribe="}"#)
                .as_deref(),
            Some("pattern_removed")
        );
        assert_eq!(refusal_error_code(""), None);
        assert_eq!(refusal_error_code("<html>502</html>"), None);
        assert_eq!(refusal_error_code(r#"{"error":42}"#), None);
        assert_eq!(refusal_error_code(r#"{"message":"nope"}"#), None);
    }
}
