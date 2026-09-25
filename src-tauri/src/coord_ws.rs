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
//! Phase 2 closed that: every lane presents a device JWT and names one of
//! coord's CLOSED set of subscriptions (`?subscribe=<name>`), which coord maps
//! server-side to the pattern the TOKEN's principal is entitled to
//! (qontinui-coord `crates/coord/src/ws.rs`, `resolve_subscription`). A
//! `?subscribe=` upgrade with NO token is refused 403
//! `subscription_scope_required`; a token that is present but bad is refused
//! as a credential failure, never downgraded to anonymous. `?pattern=` is no
//! longer how the scope is chosen: alongside a token it only narrows the
//! principal's entitlement, and WITHOUT one it is admitted only inside a
//! legacy allowlist (`anonymous_entitlement`) and refused 403
//! `pattern_out_of_scope` outside it. This module never sends `?pattern=`.
//!
//! This module is the single place that knows both halves:
//!
//! - [`build_ws_url`] — scheme swap, idempotent `/ws`, `?subscribe=<name>`;
//! - [`connect`] — resolve the device JWT PER CONNECT and present it as
//!   `?token=` AND `Authorization: Bearer`, BOUND the attempt by
//!   [`CONNECT_TIMEOUT`], then name any refusal (status + coord's `error`
//!   code) so a 401/403 reconnect flap is diagnosable from the log, and hand
//!   a 401 to the device-JWT refresher through [`set_unauthorized_hook`].
//!
//! The last two are what makes a shared connect worth having over four
//! copies: a hang bound and a credential-recovery kick are each one edit
//! here rather than four, and neither can drift between lanes.
//!
//! It lives in the lib crate because `env_agent` does, and the bin crate's
//! lanes reach it as `qontinui_runner_lib::coord_ws`. The bearer accessor it
//! calls is `auth::device_bearer_scoped` — the same accessor behind
//! `auth::attach_device_auth`, reading the on-disk credential slot per call,
//! so a reconnect after the ~4 h device-JWT rollover presents the fresh token.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tokio_tungstenite::tungstenite;
use tracing::warn;

/// Hard ceiling on ONE coord `/ws` connect attempt (TCP + TLS + HTTP
/// upgrade).
///
/// `connect_async` has **no** built-in timeout and is not interruptible
/// while parked, so a half-open or black-holed socket parks the calling
/// lane's reconnect loop forever: no error, no backoff, no log line, and —
/// because every lane here is a single task around a single socket — no
/// agent spawns, no CI dispatch, no devenv directives and no session
/// handoffs for the life of the process. The runner's other WS client
/// records that exact failure being observed in production
/// (`mcp::backend_relay::CONNECT_TIMEOUT`: *"would block the relay task
/// indefinitely with no recovery"*); these four lanes had the same exposure
/// and no bound. Same 20 s budget as that client, deliberately: it is one
/// property with one value, and a lane that timed out differently from the
/// relay would be a second thing to reason about.
///
/// Exceeding it surfaces as an ordinary `Error::Io(TimedOut)`, which every
/// lane's existing `Err` arm already handles as a transport failure — so the
/// recovery is the reconnect backoff that was always there, not new
/// machinery. [`upgrade_refusal_is_unauthorized`] answers `false` for it, so
/// a slow network never kicks the credential refresher.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Shortest interval between two [`unauthorized_hook`] firings, process-wide.
///
/// Four lanes reconnect independently with a 2 s backoff floor, so one stale
/// device JWT produces a 401 on each of them several times a minute. The
/// hook's job is to tell the refresher to look NOW, and telling it once is
/// the whole of that job — an unfloored kick would turn one expired
/// credential into a mint storm against coord. The same bounding instinct as
/// `backend_relay`'s consecutive-rejection kick ceiling, expressed as a rate
/// rather than a count because these lanes never stop retrying.
const UNAUTHORIZED_KICK_FLOOR: Duration = Duration::from_secs(60);

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
///
/// `pub(crate)`, not `pub`: the carriage is [`connect`]'s business and no
/// lane assembles a credentialed URL itself — one that did would be holding a
/// token it must then keep out of its own logs, which is the hazard this
/// module exists to centralise.
pub(crate) fn ws_url_with_token(ws_url: &str, token: &str) -> String {
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
    let attempt = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async(request),
    )
    .await
    {
        Ok(inner) => inner,
        // Render the expired budget as the transport error it is, so the
        // caller's existing `Err` arm backs off and retries exactly as it
        // would for a refused connection. See [`CONNECT_TIMEOUT`].
        Err(_elapsed) => Err(tungstenite::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "coord /ws connect exceeded {}s with no handshake response",
                CONNECT_TIMEOUT.as_secs()
            ),
        ))),
    };
    match attempt {
        Ok((ws, _resp)) => Ok(ws),
        Err(e) => {
            log_upgrade_failure(lane, &e);
            if upgrade_refusal_is_unauthorized(&e) {
                kick_unauthorized_hook(lane);
            }
            Err(anyhow::anyhow!("WS upgrade: {e}"))
        }
    }
}

/// Install the process-wide callback [`connect`] fires when coord refuses a
/// `/ws` upgrade **401**. Idempotent: the first installation wins and later
/// calls are ignored, so a double-install cannot produce a double kick.
///
/// Why a hook rather than a direct call: the recovery is the device-JWT
/// refresher (`mcp::device_jwt_refresher`), which lives in the BIN crate,
/// while this module lives in the lib crate that `env_agent` needs — so the
/// dependency can only run bin→lib. `main.rs` installs the kick once at
/// startup and all four lanes inherit it; leaving the hook uninstalled (every
/// test binary, and `env_agent` used standalone) degrades to today's
/// behaviour, which is the reconnect flap plus the refresher's own ~5 min
/// poll.
///
/// Why it exists at all: before Phase 2 these upgrades carried no credential,
/// so a 401 was not reachable and there was nothing to refresh. Making them
/// credential-bearing created the stale-JWT failure mode in these four lanes
/// for the first time, and the runner already knows what to do about it —
/// `backend_relay` kicks the refresher on its own 401 rather than waiting out
/// the poll (`mcp::backend_relay::is_unauthorized`). This wires the same
/// recovery to the lanes that just acquired the same failure mode.
pub fn set_unauthorized_hook(hook: fn()) {
    let _ = unauthorized_hook().set(hook);
}

fn unauthorized_hook() -> &'static OnceLock<fn()> {
    static HOOK: OnceLock<fn()> = OnceLock::new();
    &HOOK
}

/// Millis since [`process_epoch`] at which the NEXT hook firing becomes due.
///
/// A DEADLINE rather than a "last fired" timestamp, and that is the whole
/// reason it is correct. A "last fired" counter would need `0` to mean "never
/// fired", and `now_ms == 0` is not a rare first-millisecond edge case here —
/// because [`process_epoch`] is initialised lazily by the very same call, it
/// is the value of the FIRST kick, every time, in every process. So the
/// sentinel would collide on the first kick always: that kick would never
/// advance the floor, and every refusal after it would kick again, defeating
/// the rate limit entirely. A deadline starting at `0` needs no sentinel —
/// every `now_ms` is `>= 0`, so the first refusal is due whenever it lands.
fn next_unauthorized_kick_due() -> &'static AtomicU64 {
    static NEXT_DUE: AtomicU64 = AtomicU64::new(0);
    &NEXT_DUE
}

/// The monotonic origin [`UNAUTHORIZED_KICK_FLOOR`] is measured against.
///
/// NOT process start: `OnceLock::get_or_init` runs on the FIRST call, and the
/// only caller is [`kick_unauthorized_hook`] past its hook guard — so this is
/// the instant of the first 401 seen with a hook installed, which may be hours
/// into the process. That is fine, and it is what makes the deadline model in
/// [`next_unauthorized_kick_due`] load-bearing rather than defensive. What
/// matters here is only that it is an [`Instant`]: monotonic, so a
/// system-clock step cannot move the floor.
fn process_epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

/// Fire the installed hook unless one fired within
/// [`UNAUTHORIZED_KICK_FLOOR`]. No hook installed → nothing happens, and the
/// floor is not consumed (hence the guard order: hook lookup FIRST).
///
/// One case that guard does not cover, stated rather than hidden: `main.rs`
/// installs the hook in the first lines of `main()`, while the refresher it
/// kicks is registered later in setup and `kick_device_jwt_refresher` no-ops
/// until then. A 401 landing in that window claims the floor for a kick that
/// did nothing, swallowing 401s for one [`UNAUTHORIZED_KICK_FLOOR`]. Bounded
/// at 60 s, still far better than the ~5 min poll this replaces, and not worth
/// a readiness handshake between the two crates to close.
fn kick_unauthorized_hook(lane: &str) {
    let Some(hook) = unauthorized_hook().get() else {
        return;
    };
    let now_ms = process_epoch().elapsed().as_millis() as u64;
    if !claim_unauthorized_kick(
        next_unauthorized_kick_due(),
        now_ms,
        UNAUTHORIZED_KICK_FLOOR,
    ) {
        return;
    }
    warn!(
        "{lane}: coord /ws upgrade refused 401 — asking the device-JWT refresher to \
         re-mint now rather than waiting out its poll"
    );
    hook();
}

/// Pure rate floor: `true` — and `next_due` is pushed out by `floor` — iff
/// `now_ms` has reached the stored deadline. Split out so the bound is
/// unit-testable without a clock or a hook.
fn claim_unauthorized_kick(next_due: &AtomicU64, now_ms: u64, floor: Duration) -> bool {
    let mut prev = next_due.load(Ordering::Relaxed);
    loop {
        if now_ms < prev {
            return false;
        }
        let pushed = now_ms.saturating_add(floor.as_millis() as u64);
        // CAS rather than a plain store: four lanes can refuse concurrently,
        // and exactly one of them should carry the kick through.
        match next_due.compare_exchange_weak(prev, pushed, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => prev = observed,
        }
    }
}

/// Whether `e` is coord refusing the upgrade with **401**, the one shape a
/// fresher device JWT can fix.
///
/// Mirrors `mcp::backend_relay::is_unauthorized` deliberately, including what
/// it excludes: a 403 is a policy verdict about this principal's
/// subscriptions, a 400 is a request-shape complaint, a 5xx is coord's
/// problem, and a transport error (including [`CONNECT_TIMEOUT`] expiring)
/// carries no verdict at all. A new credential fixes none of them, so none of
/// them kicks the refresher.
///
/// `pub(crate)` for the same reason as [`log_upgrade_failure`] beside it:
/// [`connect`] is the only caller, and the only way a lane obtains one of
/// these errors at all.
pub(crate) fn upgrade_refusal_is_unauthorized(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Http(resp) if resp.status().as_u16() == 401)
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
             credential; coord refuses a tokenless `?subscribe=` upgrade 403 \
             subscription_scope_required"
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
/// HTTP status plus both halves of coord's JSON refusal body. Coord's refusals
/// are `{"error": "<message>", "code": "<code>", …}`: `code=` is the machine
/// code the hints below name (`subscription_scope_required`, …) and `error=` is
/// the human-readable message that says which case of a hint applies. Never
/// the request — the URL carries the token.
///
/// `pub(crate)`, not `pub`: [`connect`] is the only caller and the only way a
/// lane obtains one of these errors, so a `pub` spelling advertised an entry
/// point into this module that does not exist.
pub(crate) fn log_upgrade_failure(lane: &str, e: &tungstenite::Error) {
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
                    " — the upgrade request itself was rejected before coord's /ws handler \
                        answered (coord's scope refusals are 403, not 400); check for a proxy \
                        rewrite of the URL"
                }
                401 => {
                    " — a device JWT was presented and rejected (expired, from another coord, \
                        bad signature or otherwise invalid, revoked, or coord holding no keys; \
                        the error message says which); a MISSING JWT is refused 403 instead"
                }
                403 => {
                    " — not admitted: no credential was sent (coord code \
                        subscription_scope_required), the credential verified but is a \
                        capability grant rather than a principal, or the principal cannot \
                        resolve this subscription"
                }
                _ => "",
            };
            warn!(
                "{lane}: coord /ws upgrade refused status={status} code={} error={}{hint}",
                refusal_error_code(&body).as_deref().unwrap_or("<none>"),
                refusal_field(&body, "error").as_deref().unwrap_or("<none>"),
            );
        }
        other => warn!("{lane}: coord /ws upgrade failed before any response: {other}"),
    }
}

/// The machine code of a coord refusal body. Coord's credential refusals
/// (`auth.rs` `Refusal`) carry it as `"code"` beside a human-readable
/// `"error"` message; older refusals carry the code AS `"error"` and send no
/// `"code"`. So `"code"` wins and `"error"` is the fallback — a caller that
/// branches on the result (`agent_not_found`, …) matches either shape, and
/// never compares a code against a sentence. `None` for an empty body, a
/// non-JSON body (an ALB or proxy page), or a JSON body with neither string
/// field.
pub fn refusal_error_code(body: &str) -> Option<String> {
    refusal_field(body, "code").or_else(|| refusal_field(body, "error"))
}

/// One string field of a JSON refusal body; `None` when the body is not a
/// JSON object or the field is absent or not a string.
fn refusal_field(body: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get(key)?
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

    /// Only a 401 kicks the refresher. Everything else — a policy 403, a
    /// shape 400, coord's own 5xx, and every transport error including the
    /// [`CONNECT_TIMEOUT`] expiry — carries no verdict a fresh credential
    /// changes.
    #[test]
    fn only_a_401_upgrade_refusal_is_unauthorized() {
        let http = |status: u16| {
            let resp = tungstenite::http::Response::builder()
                .status(status)
                .body(None)
                .unwrap();
            tungstenite::Error::Http(Box::new(resp))
        };
        assert!(upgrade_refusal_is_unauthorized(&http(401)));
        for status in [400, 403, 404, 429, 500, 502, 503] {
            assert!(
                !upgrade_refusal_is_unauthorized(&http(status)),
                "status {status} must not kick the refresher"
            );
        }
        assert!(!upgrade_refusal_is_unauthorized(&tungstenite::Error::Io(
            std::io::Error::new(std::io::ErrorKind::TimedOut, "connect budget")
        )));
        assert!(!upgrade_refusal_is_unauthorized(
            &tungstenite::Error::ConnectionClosed
        ));
    }

    /// The very first refusal kicks even when it lands in the process's
    /// first MILLISECOND (`now_ms == 0`) — and having kicked, it advances the
    /// floor. A "last fired" counter with `0` meaning "never" gets this
    /// wrong in both halves: it kicks, stores 0, still reads as never-fired,
    /// and then kicks on every refusal that follows.
    #[test]
    fn the_first_refusal_kicks_at_t0_and_still_advances_the_floor() {
        let next_due = AtomicU64::new(0);
        assert!(claim_unauthorized_kick(
            &next_due,
            0,
            UNAUTHORIZED_KICK_FLOOR
        ));
        assert!(!claim_unauthorized_kick(
            &next_due,
            1,
            UNAUTHORIZED_KICK_FLOOR
        ));
        assert!(!claim_unauthorized_kick(
            &next_due,
            UNAUTHORIZED_KICK_FLOOR.as_millis() as u64 - 1,
            UNAUTHORIZED_KICK_FLOOR
        ));
        assert!(claim_unauthorized_kick(
            &next_due,
            UNAUTHORIZED_KICK_FLOOR.as_millis() as u64,
            UNAUTHORIZED_KICK_FLOOR
        ));
    }

    /// Four lanes flapping at the 2 s backoff floor must produce ONE kick per
    /// [`UNAUTHORIZED_KICK_FLOOR`], not one per refusal.
    #[test]
    fn concurrent_lane_refusals_collapse_to_one_kick_per_floor() {
        let next_due = AtomicU64::new(0);
        let floor = Duration::from_secs(60);
        let mut kicks = 0;
        // 150 s of four lanes each refusing every 2 s: 300 refusals.
        for t_ms in (0..150_000).step_by(2_000) {
            for _lane in 0..4 {
                if claim_unauthorized_kick(&next_due, t_ms, floor) {
                    kicks += 1;
                }
            }
        }
        // t=0, t=60s, t=120s — and nothing in between.
        assert_eq!(kicks, 3, "expected one kick per 60s floor over 150s");
    }

    /// The floor is a rate, not a one-shot: a credential that goes stale
    /// again hours later still gets a kick.
    #[test]
    fn a_later_refusal_past_the_floor_kicks_again() {
        let next_due = AtomicU64::new(0);
        let floor = Duration::from_secs(60);
        assert!(claim_unauthorized_kick(&next_due, 1_000, floor));
        assert!(!claim_unauthorized_kick(&next_due, 60_999, floor));
        assert!(claim_unauthorized_kick(&next_due, 61_000, floor));
    }

    /// With no hook installed the floor must stay unconsumed, or the first
    /// refusals of a process would silently spend the budget the hook is
    /// installed to use.
    #[test]
    fn no_hook_installed_does_not_consume_the_floor() {
        // `unauthorized_hook()` is process-global and this test must not
        // install one; it asserts the guard ORDER in `kick_unauthorized_hook`
        // (hook lookup first, floor second) by calling it and observing the
        // deadline is untouched.
        let before = next_unauthorized_kick_due().load(Ordering::Relaxed);
        if unauthorized_hook().get().is_none() {
            kick_unauthorized_hook("test");
            assert_eq!(next_unauthorized_kick_due().load(Ordering::Relaxed), before);
        }
    }

    /// The bound is the same one the runner's other WS client applies, and it
    /// is a value this module OWNS rather than re-derives per lane.
    #[test]
    fn connect_timeout_is_bounded_and_matches_the_relay_budget() {
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(20));
    }

    #[test]
    fn refusal_error_code_prefers_code_over_the_error_message() {
        let body = r#"{"error":"missing ?token= query param","code":"missing_token"}"#;
        assert_eq!(refusal_error_code(body).as_deref(), Some("missing_token"));
        assert_eq!(
            refusal_field(body, "error").as_deref(),
            Some("missing ?token= query param")
        );
        // A non-string `code` is not a code; the `error` fallback still applies.
        assert_eq!(
            refusal_error_code(r#"{"error":"agent_not_found","code":7}"#).as_deref(),
            Some("agent_not_found")
        );
    }

    #[test]
    fn refusal_error_code_falls_back_to_a_string_error_field() {
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
