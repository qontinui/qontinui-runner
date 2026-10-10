//! The runner's outbound-network module: proxy resolution, the proxy-aware
//! WebSocket connect, and the startup step that exports the machine profile's
//! network settings into the process environment.
//!
//! Plan `2026-10-10-spec-front-end-phase-9-generic-boundary`, Phase 4
//! (decisions C1 and C2).
//!
//! # Why this exists
//!
//! `reqwest` honours `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` (and, through
//! `hyper-util`'s `client-proxy-system`, the Windows and macOS system proxy)
//! by default, so the runner's HTTP traffic already passes through a corporate
//! proxy. Its three long-lived WebSockets did not:
//! `tokio_tungstenite::connect_async` opens a direct TCP connection and has no
//! proxy support at all. Behind a mandatory proxy the coord `/ws` lanes, the
//! web relay and the cloud tunnel therefore never connected — and they are the
//! runner's only path for live coordination, the web console and remote
//! access.
//!
//! [`connect_ws`] is the ONE WebSocket connect in the runner. It asks the same
//! `hyper_util::client::proxy::matcher::Matcher` reqwest uses whether the
//! destination is intercepted, so HTTP and WebSocket traffic always agree
//! about which proxy applies — agreement is a property of shared code, not of
//! a parallel copy of the `NO_PROXY` rules. A source-scan test
//! (`tests::no_connect_async_outside_outbound_net`) forbids `connect_async(`
//! anywhere else in production code.
//!
//! # Loopback always bypasses
//!
//! `127.0.0.1`, `::1` and `localhost` are never proxied by [`connect_ws`],
//! checked before the matcher, and [`apply_profile_environment`] always puts
//! them in the exported `NO_PROXY`, so reqwest, `git` and child processes skip
//! any proxy — environment OR OS system proxy — for them too. A proxy can
//! therefore never break the runner's calls to itself. (`hyper-util` has no
//! implicit loopback bypass, and the WinINet `<local>` entry matches nothing
//! there, so without this a Windows or macOS system proxy would receive
//! `http://127.0.0.1:*`.)
//!
//! Exporting `NO_PROXY` switches off `hyper-util`'s own read of the Windows
//! registry bypass list (`ProxyOverride`), which it consults only while
//! `NO_PROXY` is unset. So when the operator set no `NO_PROXY`, the exported
//! list is a strict SUPERSET of that registry list (converted the way
//! `hyper-util` converts it) plus the profile's `network.no_proxy` plus
//! loopback: every host IT exempted stays exempt. When the operator DID set
//! `NO_PROXY`, `hyper-util` was never going to read the registry list, so it
//! is not folded in; the operator's entries are kept and the profile's entries
//! and loopback are appended. An operator `NO_PROXY=*` is left untouched.
//!
//! # Proxies the WebSocket path cannot use
//!
//! Only an `http://` proxy that accepts `CONNECT` can carry a WebSocket here.
//! A `socks5://` or `https://` proxy is skipped with ONE warning per process
//! and the socket goes direct — the behaviour before this module existed —
//! rather than failing every connect. The config report's `network_proxy` row
//! says so.
//!
//! # Corporate CA trust (Phase 5)
//!
//! The same startup step carries decision C3. When the profile names
//! `network.ca_bundle` it exports `NODE_EXTRA_CA_CERTS`, which is ADDITIVE
//! (Node keeps its own roots and adds the file's). It deliberately does NOT
//! export `SSL_CERT_FILE`: on Linux that variable REPLACES the OS store for
//! rustls-native-certs and OpenSSL, in this process and every child. On
//! Windows, only when the profile says `network.trust: "os"` explicitly AND
//! the machine's git config names neither `http.sslBackend` nor
//! `http.sslCAInfo`, it appends `http.sslBackend=schannel` to the
//! `GIT_CONFIG_*` environment so the runner's `git` trusts the Windows store;
//! otherwise git is left on whatever the machine chose. [`tls_trust`] holds
//! the per-stack census.
//!
//! # What is never logged
//!
//! Neither the proxy credential nor the request URL. The coord `/ws` URL and
//! the cloud-tunnel URL both carry `?token=` (see `coord_ws`), and a proxy URL
//! may carry `user:password@`. Log lines name hosts and ports only.

use std::sync::OnceLock;
use std::time::Duration;

use hyper_util::client::proxy::matcher::Matcher;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{
    self,
    handshake::client::Request,
    http::{HeaderValue, Uri},
};
use tracing::{debug, info, warn};

use crate::coord_ws::CoordWs;
use crate::profiles::NetworkProfile;
pub use crate::profiles::TrustMode;

/// Corporate CA trust: the per-stack census and its measurements (Phase 5).
pub mod tls_trust;

/// Upper bound on the proxy's CONNECT response (status line plus headers).
/// A proxy that answers with more than this is not speaking HTTP/1.1 CONNECT
/// in any form worth tunnelling through.
const CONNECT_RESPONSE_LIMIT: usize = 8 * 1024;

/// The three loopback spellings that always bypass a proxy (decision C2).
pub const LOOPBACK_NO_PROXY: [&str; 3] = ["127.0.0.1", "::1", "localhost"];

// ===========================================================================
// Proxy routing
// ===========================================================================

/// A resolved proxy hop for one WebSocket destination.
///
/// `Debug` deliberately omits the `Proxy-Authorization` value.
#[derive(Clone)]
pub struct ProxyRoute {
    proxy_scheme: String,
    proxy_host: String,
    proxy_port: u16,
    authorization: Option<HeaderValue>,
    target_host: String,
    target_port: u16,
}

impl std::fmt::Debug for ProxyRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyRoute")
            .field("proxy_scheme", &self.proxy_scheme)
            .field("proxy_host", &self.proxy_host)
            .field("proxy_port", &self.proxy_port)
            .field(
                "authorization",
                &self.authorization.as_ref().map(|_| "<withheld>"),
            )
            .field("target_host", &self.target_host)
            .field("target_port", &self.target_port)
            .finish()
    }
}

impl ProxyRoute {
    /// The proxy hop for a `ws://` / `wss://` destination, or `None` when the
    /// connection goes direct.
    ///
    /// `wss` is asked as `https` and `ws` as `http`, so the
    /// `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` / `NO_PROXY` rules — and,
    /// through [`Matcher::from_system`], the OS system proxy — are exactly the
    /// ones reqwest applies. Loopback is checked first and always bypasses.
    pub fn for_url(url: &Uri, matcher: &Matcher) -> Option<ProxyRoute> {
        let (http_scheme, default_port) = match url.scheme_str()? {
            "wss" | "https" => ("https", 443),
            "ws" | "http" => ("http", 80),
            _ => return None,
        };
        let raw_host = url.host()?;
        if is_loopback_host(raw_host) {
            return None;
        }
        let target_port = url.port_u16().unwrap_or(default_port);
        let probe: Uri = format!("{http_scheme}://{}/", authority(raw_host, target_port))
            .parse()
            .ok()?;
        let intercept = matcher.intercept(&probe)?;
        let proxy_uri = intercept.uri();
        let proxy_scheme = proxy_uri
            .scheme_str()
            .unwrap_or("http")
            .to_ascii_lowercase();
        let proxy_host = strip_brackets(proxy_uri.host()?).to_string();
        let proxy_port =
            proxy_uri
                .port_u16()
                .unwrap_or(if proxy_scheme == "https" { 443 } else { 80 });
        Some(ProxyRoute {
            proxy_scheme,
            proxy_host,
            proxy_port,
            authorization: intercept.basic_auth().cloned(),
            target_host: strip_brackets(raw_host).to_string(),
            target_port,
        })
    }

    /// The proxy's `host:port` — safe to log (never carries the credential).
    pub fn proxy_authority(&self) -> String {
        authority(&self.proxy_host, self.proxy_port)
    }

    /// The `host:port` the CONNECT asks for.
    pub fn target_authority(&self) -> String {
        authority(&self.target_host, self.target_port)
    }

    /// True when the WebSocket path can tunnel through this proxy (an
    /// `http://` proxy that accepts `CONNECT`).
    pub fn is_tunnellable(&self) -> bool {
        proxy_scheme_is_tunnellable(&self.proxy_scheme)
    }

    /// Open TCP to the proxy, ask it for a `CONNECT` tunnel to the target, and
    /// return the tunnelled stream once the proxy answers 2xx.
    async fn open_tunnel(&self) -> Result<TcpStream, WsConnectError> {
        let mut stream = TcpStream::connect((self.proxy_host.as_str(), self.proxy_port))
            .await
            .map_err(|e| {
                WsConnectError::Ws(tungstenite::Error::Io(std::io::Error::new(
                    e.kind(),
                    format!(
                        "could not reach the HTTP proxy {}: {e}",
                        self.proxy_authority()
                    ),
                )))
            })?;

        let target = self.target_authority();
        let mut head = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
        if let Some(auth) = &self.authorization {
            // A HeaderValue built by hyper-util from URL userinfo is always
            // visible ASCII (`Basic <base64>`).
            if let Ok(value) = auth.to_str() {
                head.push_str("Proxy-Authorization: ");
                head.push_str(value);
                head.push_str("\r\n");
            }
        }
        head.push_str("\r\n");
        stream
            .write_all(head.as_bytes())
            .await
            .map_err(|e| WsConnectError::Ws(tungstenite::Error::Io(e)))?;

        let status = read_connect_status(&mut stream).await?;
        match status {
            200..=299 => Ok(stream),
            407 => Err(WsConnectError::ProxyAuthRequired),
            status => Err(WsConnectError::ProxyRefused { status }),
        }
    }
}

/// Read the proxy's answer to `CONNECT` up to the blank line, bounded at
/// [`CONNECT_RESPONSE_LIMIT`], and return its status code.
///
/// Byte-at-a-time on purpose: everything after the blank line belongs to the
/// tunnel, so a buffered reader would swallow bytes the WebSocket handshake
/// needs. The response is a few dozen bytes, so the cost is nothing.
async fn read_connect_status(stream: &mut TcpStream) -> Result<u16, WsConnectError> {
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    loop {
        let n = stream
            .read(&mut byte)
            .await
            .map_err(|e| WsConnectError::Ws(tungstenite::Error::Io(e)))?;
        if n == 0 {
            return Err(WsConnectError::ProxyProtocol(
                "the proxy closed the connection before answering CONNECT".into(),
            ));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() >= CONNECT_RESPONSE_LIMIT {
            return Err(WsConnectError::ProxyProtocol(format!(
                "the proxy's CONNECT response exceeded {} KiB without ending its headers",
                CONNECT_RESPONSE_LIMIT / 1024
            )));
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let status_line = head.lines().next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let code = parts.next().and_then(|c| c.parse::<u16>().ok());
    match (version.starts_with("HTTP/"), code) {
        (true, Some(code)) => Ok(code),
        _ => Err(WsConnectError::ProxyProtocol(format!(
            "the proxy answered CONNECT with a status line that is not HTTP: {:?}",
            status_line.chars().take(80).collect::<String>()
        ))),
    }
}

/// Whether the WebSocket path can use a proxy of this scheme.
pub fn proxy_scheme_is_tunnellable(scheme: &str) -> bool {
    scheme.eq_ignore_ascii_case("http")
}

/// Warn once per process that a proxy of an unsupported scheme was skipped.
fn warn_untunnellable_once(route: &ProxyRoute) {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        warn!(
            "outbound_net: the configured {}:// proxy ({}) cannot carry a WebSocket — only an \
             http:// proxy that accepts CONNECT can; WebSocket transports connect DIRECT \
             (warned once per process)",
            route.proxy_scheme,
            route.proxy_authority()
        );
    }
}

fn strip_brackets(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

/// `host:port`, bracketing an IPv6 literal.
fn authority(host: &str, port: u16) -> String {
    let host = strip_brackets(host);
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// True for the three loopback spellings that always bypass a proxy.
pub fn is_loopback_host(host: &str) -> bool {
    let host = strip_brackets(host);
    host.eq_ignore_ascii_case("localhost")
        || host == "127.0.0.1"
        || host
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The process-wide matcher: environment first, then the OS system proxy —
/// the same construction reqwest uses. Built once, on first use, which is
/// always after `main` ran [`apply_profile_environment_at_startup`].
fn system_matcher() -> &'static Matcher {
    static MATCHER: OnceLock<Matcher> = OnceLock::new();
    MATCHER.get_or_init(Matcher::from_system)
}

// ===========================================================================
// The WebSocket connect
// ===========================================================================

/// Why a [`connect_ws`] attempt failed.
///
/// [`WsConnectError::Ws`] keeps tungstenite's own error, including its `Http`
/// arm, so a 401 from the origin — direct or through a tunnel — is still seen
/// by `coord_ws::upgrade_refusal_is_unauthorized` and
/// `backend_relay::is_unauthorized`. The three proxy arms are typed so an
/// operator can tell "the proxy said no" from "coord said no".
///
/// No variant's `Display` carries the request URL or a credential.
#[derive(Debug, thiserror::Error)]
pub enum WsConnectError {
    /// The WebSocket handshake or its transport failed (also: the attempt
    /// exceeded its timeout, as `Io(TimedOut)`).
    #[error("{0}")]
    Ws(#[from] tungstenite::Error),
    /// The proxy answered CONNECT with a non-2xx status other than 407.
    #[error("the HTTP proxy refused the CONNECT tunnel with status {status}")]
    ProxyRefused { status: u16 },
    /// The proxy answered CONNECT 407.
    #[error(
        "the HTTP proxy requires authentication (407) — put the credential in the proxy URL \
         (network.proxy_url in profiles.json, or HTTPS_PROXY)"
    )]
    ProxyAuthRequired,
    /// The proxy did not speak HTTP/1.1 CONNECT.
    #[error("HTTP proxy protocol error: {0}")]
    ProxyProtocol(String),
}

impl WsConnectError {
    /// The tungstenite error, when the failure was the handshake's rather than
    /// the proxy's.
    pub fn as_tungstenite(&self) -> Option<&tungstenite::Error> {
        match self {
            WsConnectError::Ws(e) => Some(e),
            _ => None,
        }
    }

    /// True only for an origin `401` on the upgrade — never for a proxy
    /// refusal (a fresh device JWT cannot satisfy a proxy).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, WsConnectError::Ws(tungstenite::Error::Http(resp)) if resp.status().as_u16() == 401)
    }
}

/// Open a WebSocket for `request`, through the HTTP proxy the process
/// environment (or the OS system proxy) names for its destination, or direct
/// when none applies. The whole attempt — proxy TCP, CONNECT, TLS, upgrade —
/// is bounded by `timeout`; exceeding it is `Ws(Io(TimedOut))`, which every
/// caller already treats as an ordinary transport failure.
pub async fn connect_ws(request: Request, timeout: Duration) -> Result<CoordWs, WsConnectError> {
    connect_ws_with(request, timeout, system_matcher()).await
}

/// [`connect_ws`] with an explicit matcher — the seam the hermetic tests use
/// so they never touch the process environment.
pub async fn connect_ws_with(
    request: Request,
    timeout: Duration,
    matcher: &Matcher,
) -> Result<CoordWs, WsConnectError> {
    match tokio::time::timeout(timeout, attempt(request, matcher)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(WsConnectError::Ws(tungstenite::Error::Io(
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "WebSocket connect exceeded {}s with no handshake response",
                    timeout.as_secs()
                ),
            ),
        ))),
    }
}

async fn attempt(request: Request, matcher: &Matcher) -> Result<CoordWs, WsConnectError> {
    match ProxyRoute::for_url(request.uri(), matcher) {
        None => {
            let (ws, _resp) = tokio_tungstenite::connect_async(request).await?;
            Ok(ws)
        }
        Some(route) if !route.is_tunnellable() => {
            warn_untunnellable_once(&route);
            let (ws, _resp) = tokio_tungstenite::connect_async(request).await?;
            Ok(ws)
        }
        Some(route) => {
            debug!(
                "outbound_net: tunnelling a WebSocket to {} through HTTP proxy {}",
                route.target_authority(),
                route.proxy_authority()
            );
            let stream = route.open_tunnel().await?;
            // TLS to the origin (for wss) runs INSIDE the tunnel, with the
            // native-tls connector, which reads the OS trust store.
            let (ws, _resp) =
                tokio_tungstenite::client_async_tls_with_config(request, stream, None, None)
                    .await?;
            Ok(ws)
        }
    }
}

// ===========================================================================
// Startup: the profile's network block -> the process environment (C2)
// ===========================================================================

/// Read/write access to an environment. The process implementation is
/// [`ProcessEnv`]; the tests use a map so they never touch the real one.
pub trait EnvAccess {
    fn get(&self, key: &str) -> Option<String>;
    fn set(&mut self, key: &str, value: &str);
}

/// The real process environment. Writing to it is sound only before any other
/// thread exists, which is why [`apply_profile_environment_at_startup`] runs
/// at the top of `main`, before the Tokio runtime is built.
pub struct ProcessEnv;

impl EnvAccess for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
    fn set(&mut self, key: &str, value: &str) {
        std::env::set_var(key, value);
    }
}

/// Which rung decided whether outbound traffic goes through a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyEnvArm {
    /// The operator's own `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` was set
    /// when the runner started; it wins over the profile.
    OperatorEnv,
    /// The active profile's `network.proxy_url` was exported.
    Profile,
    /// Neither: no environment proxy. On Windows and macOS the OS system proxy
    /// may still apply, through the same matcher reqwest uses.
    None,
}

impl ProxyEnvArm {
    pub fn as_str(self) -> &'static str {
        match self {
            ProxyEnvArm::OperatorEnv => "operator_env",
            ProxyEnvArm::Profile => "profile",
            ProxyEnvArm::None => "none",
        }
    }
}

/// What [`apply_profile_environment`] decided and wrote. Carries no
/// credential: `proxy` is the redacted display form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProxyEnvOutcome {
    pub arm: ProxyEnvArm,
    /// The effective proxy, credentials removed (`None` under
    /// [`ProxyEnvArm::None`]).
    pub proxy: Option<String>,
    /// The profile whose `network` block was consulted, if any.
    pub profile: Option<String>,
    /// The `NO_PROXY` value now in force (loopback always included).
    pub no_proxy: String,
    /// The variable NAMES this step wrote.
    pub exported: Vec<String>,
    /// The profile's `network.trust`, as configured (`None` = not set).
    pub trust: Option<TrustMode>,
    /// The CA bundle named by the profile (exported to Node as
    /// `NODE_EXTRA_CA_CERTS` unless the operator set one).
    pub ca_bundle: Option<String>,
    /// The `http.sslBackend` this step pointed git at, when it did.
    pub git_ssl_backend: Option<String>,
    /// Why git was or was not pointed at Schannel.
    pub git_trust: GitTrustDecision,
    /// `NODE_EXTRA_CA_CERTS` as it stands after this step — the operator's
    /// own value or the exported bundle — i.e. what Node actually adds.
    pub node_extra_ca_certs: Option<String>,
}

const OPERATOR_PROXY_VARS: [&str; 6] = [
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

fn non_empty(env: &impl EnvAccess, key: &str) -> Option<String> {
    env.get(key).filter(|v| !v.trim().is_empty())
}

/// Export the profile's network settings into `env` (decision C2).
///
/// - `HTTPS_PROXY` and `HTTP_PROXY` are set from `network.proxy_url` only when
///   the operator set no proxy variable of their own — an explicit operator
///   value always wins.
/// - `NO_PROXY` (and its lowercase twin, which curl and so `git` read first)
///   becomes the union of what was there, the profile's `no_proxy`, and the
///   three loopback spellings.
pub fn apply_profile_environment(
    network: Option<&NetworkProfile>,
    profile: Option<&str>,
    env: &mut impl EnvAccess,
) -> ProxyEnvOutcome {
    apply_profile_environment_with(network, profile, env, &SystemInputs::current())
}

/// What the startup step reads from the machine rather than the environment.
/// A parameter so every arm is testable on every host.
pub struct SystemInputs<'a> {
    /// The runner is on Windows (git's Schannel step applies only there).
    pub windows: bool,
    /// The Windows registry bypass list (`ProxyOverride`), already converted
    /// the way `hyper-util` converts it, and only while `ProxyEnable` is on —
    /// exactly when `hyper-util` itself would consult it.
    pub system_bypass: Option<String>,
    /// Does the machine's git config already choose `http.sslBackend` or
    /// `http.sslCAInfo`? `None` when that could not be established. Called
    /// only when the Schannel step would otherwise apply.
    pub git_tls_configured: &'a dyn Fn() -> Option<bool>,
}

impl SystemInputs<'static> {
    /// This machine.
    pub fn current() -> Self {
        SystemInputs {
            windows: cfg!(windows),
            system_bypass: windows_system_bypass(),
            git_tls_configured: &machine_git_tls_configured,
        }
    }
}

/// Why the startup step did or did not point git at Schannel — the census
/// prints it, so "git trusts X" is never asserted without its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitTrustDecision {
    /// Not Windows: the runner never touches git's TLS backend.
    NotWindows,
    /// The profile does not say `network.trust: "os"`; git keeps whatever the
    /// machine chose.
    NotRequested,
    /// `network.trust: "bundled"`.
    Bundled,
    /// The machine's git config already names `http.sslBackend` or
    /// `http.sslCAInfo`, or the operator's `GIT_CONFIG_*` does; theirs stands.
    OperatorConfigured,
    /// The machine's git config could not be read, so it was left alone.
    ConfigUnreadable,
    /// `GIT_CONFIG_COUNT` was set but not a number; git would refuse it.
    UnreadableCount,
    /// `http.sslBackend=schannel` was appended to `GIT_CONFIG_*`.
    Schannel,
}

impl GitTrustDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            GitTrustDecision::NotWindows => "not_windows",
            GitTrustDecision::NotRequested => "not_requested",
            GitTrustDecision::Bundled => "bundled",
            GitTrustDecision::OperatorConfigured => "operator_configured",
            GitTrustDecision::ConfigUnreadable => "config_unreadable",
            GitTrustDecision::UnreadableCount => "unreadable_count",
            GitTrustDecision::Schannel => "schannel",
        }
    }
}

/// [`apply_profile_environment`] with the machine's inputs as a parameter.
pub fn apply_profile_environment_with(
    network: Option<&NetworkProfile>,
    profile: Option<&str>,
    env: &mut impl EnvAccess,
    system: &SystemInputs<'_>,
) -> ProxyEnvOutcome {
    let mut exported = Vec::new();

    // The git probe spawns a subprocess (and pipe-reader threads), so it runs
    // FIRST: every environment write below happens after it has finished.
    let trust = network.and_then(|n| n.trust);
    let git_probe =
        (system.windows && trust == Some(TrustMode::Os)).then(|| (system.git_tls_configured)());

    let operator = OPERATOR_PROXY_VARS.iter().find_map(|k| non_empty(env, k));
    let profile_proxy = network
        .and_then(|n| n.proxy_url.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    let (arm, proxy) = match (operator, profile_proxy) {
        (Some(op), _) => (ProxyEnvArm::OperatorEnv, Some(redact_proxy_url(&op))),
        (None, Some(p)) => {
            for key in ["HTTPS_PROXY", "HTTP_PROXY"] {
                env.set(key, p);
                exported.push(key.to_string());
            }
            (ProxyEnvArm::Profile, Some(redact_proxy_url(p)))
        }
        (None, None) => (ProxyEnvArm::None, None),
    };

    let existing = non_empty(env, "NO_PROXY").or_else(|| non_empty(env, "no_proxy"));
    let operator_proxies_nothing = existing
        .as_deref()
        .is_some_and(|v| v.split(',').any(|e| e.trim() == "*"));
    let no_proxy = if operator_proxies_nothing {
        // `*`: the operator already exempted everything.
        existing.unwrap_or_default()
    } else {
        let mut entries: Vec<String> = Vec::new();
        let mut push = |entry: &str| {
            let entry = entry.trim();
            if !entry.is_empty() && !entries.iter().any(|e| e.eq_ignore_ascii_case(entry)) {
                entries.push(entry.to_string());
            }
        };
        // The registry list stands in for an operator NO_PROXY only when there
        // is none: that is exactly when hyper-util would have read it.
        let base = existing.as_deref().or(system.system_bypass.as_deref());
        for source in [base, network.and_then(|n| n.no_proxy.as_deref())] {
            for e in source.unwrap_or_default().split(',') {
                push(e);
            }
        }
        for e in LOOPBACK_NO_PROXY {
            push(e);
        }
        let no_proxy = entries.join(",");
        for key in ["NO_PROXY", "no_proxy"] {
            if env.get(key).as_deref() != Some(no_proxy.as_str()) {
                env.set(key, &no_proxy);
                exported.push(key.to_string());
            }
        }
        no_proxy
    };

    // Node cannot read the OS store; NODE_EXTRA_CA_CERTS ADDS the file's roots
    // to its own. SSL_CERT_FILE is deliberately NOT set: it would REPLACE the
    // OS store for this process and every child on Linux. An operator's own
    // value wins.
    let ca_bundle = network
        .and_then(|n| n.ca_bundle.as_ref())
        .map(|p| p.display().to_string())
        .filter(|p| !p.trim().is_empty());
    if let Some(bundle) = &ca_bundle {
        if non_empty(env, "NODE_EXTRA_CA_CERTS").is_none() {
            env.set("NODE_EXTRA_CA_CERTS", bundle);
            exported.push("NODE_EXTRA_CA_CERTS".to_string());
        }
    }
    let node_extra_ca_certs = non_empty(env, "NODE_EXTRA_CA_CERTS");

    // git on Windows, only on an explicit `network.trust: "os"` and only when
    // the machine's git config has not chosen a TLS backend or CA file.
    let git_trust = if !system.windows {
        GitTrustDecision::NotWindows
    } else {
        match trust {
            None => GitTrustDecision::NotRequested,
            Some(TrustMode::Bundled) => GitTrustDecision::Bundled,
            Some(TrustMode::Os) => match git_probe.flatten() {
                None => GitTrustDecision::ConfigUnreadable,
                Some(true) => GitTrustDecision::OperatorConfigured,
                Some(false) => append_git_config(env, "http.sslBackend", "schannel", &mut exported),
            },
        }
    };
    let git_ssl_backend = (git_trust == GitTrustDecision::Schannel).then(|| "schannel".to_string());

    ProxyEnvOutcome {
        arm,
        proxy,
        profile: network.and(profile).map(str::to_string),
        no_proxy,
        exported,
        trust,
        ca_bundle,
        git_ssl_backend,
        git_trust,
        node_extra_ca_certs,
    }
}

/// Convert a WinINet `ProxyOverride` value the way `hyper-util` does:
/// `;`-separated → `,`-separated, every `*.` removed.
pub fn convert_proxy_override(raw: &str) -> String {
    raw.split(';')
        .map(|s| s.trim())
        .collect::<Vec<&str>>()
        .join(",")
        .replace("*.", "")
}

/// The Windows per-user registry bypass list, converted, while the system
/// proxy is enabled (`ProxyEnable` non-zero) — the only state in which
/// `hyper-util` consults it. `None` elsewhere.
pub fn windows_system_bypass() -> Option<String> {
    #[cfg(windows)]
    {
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let key = hkcu
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            .ok()?;
        let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
        if enabled == 0 {
            return None;
        }
        let raw: String = key.get_value("ProxyOverride").ok()?;
        let converted = convert_proxy_override(&raw);
        (!converted.trim().is_empty()).then_some(converted)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The git config keys that mean "the machine chose git's TLS trust".
/// Matches the plain and the URL-scoped (`http.<url>.sslCAInfo`) spellings.
const GIT_TLS_KEYS_PATTERN: &str = "^http\\.(.+\\.)?(sslbackend|sslcainfo)$";

/// Whether the machine's git config (system, global, or this directory's)
/// names `http.sslBackend` or `http.sslCAInfo`. `None` when git could not be
/// run or answered something other than "found" / "not found".
fn machine_git_tls_configured() -> Option<bool> {
    let mut cmd = crate::process_helpers::scrubbed_git(std::ffi::OsStr::new("git"));
    cmd.args(["config", "--get-regexp", GIT_TLS_KEYS_PATTERN]);
    let out = crate::process_helpers::output_with_timeout_labeled(
        cmd,
        Duration::from_secs(5),
        "outbound_net: git config --get-regexp http.ssl*",
    )
    .ok();
    // The helper's pipe readers are detached threads that finish on EOF; let
    // them drain (bounded) so no helper thread is alive when `main` writes the
    // environment right after this returns.
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while crate::process_helpers::live_pipe_readers() > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    match out?.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// Append `key=value` to git's environment config (`GIT_CONFIG_COUNT` /
/// `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`), AFTER any entries already
/// there. [`GitTrustDecision::Schannel`] when written;
/// [`GitTrustDecision::OperatorConfigured`] when the operator's own entries
/// already set `key` (their choice stands); [`GitTrustDecision::UnreadableCount`]
/// when an existing `GIT_CONFIG_COUNT` is not a number (git itself would
/// refuse that environment, and appending to it cannot repair it).
fn append_git_config(
    env: &mut impl EnvAccess,
    key: &str,
    value: &str,
    exported: &mut Vec<String>,
) -> GitTrustDecision {
    let count = match non_empty(env, "GIT_CONFIG_COUNT") {
        None => 0usize,
        Some(raw) => match raw.trim().parse::<usize>() {
            Ok(n) => n,
            Err(_) => return GitTrustDecision::UnreadableCount,
        },
    };
    let already = (0..count).any(|i| {
        env.get(&format!("GIT_CONFIG_KEY_{i}"))
            .is_some_and(|k| k.trim().eq_ignore_ascii_case(key))
    });
    if already {
        return GitTrustDecision::OperatorConfigured;
    }
    let key_var = format!("GIT_CONFIG_KEY_{count}");
    let value_var = format!("GIT_CONFIG_VALUE_{count}");
    env.set(&key_var, key);
    env.set(&value_var, value);
    env.set("GIT_CONFIG_COUNT", &(count + 1).to_string());
    exported.extend([key_var, value_var, "GIT_CONFIG_COUNT".to_string()]);
    GitTrustDecision::Schannel
}

/// A proxy URL with any `user:password@` removed, for display. A value that
/// does not parse as a URL is withheld whole rather than printed raw.
pub fn redact_proxy_url(raw: &str) -> String {
    let candidate = if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{raw}")
    };
    match url::Url::parse(&candidate) {
        Ok(mut u) => {
            let had_credentials = !u.username().is_empty() || u.password().is_some();
            let _ = u.set_username("");
            let _ = u.set_password(None);
            let mut shown = u.to_string();
            if shown.ends_with('/') && !raw.ends_with('/') {
                shown.pop();
            }
            if had_credentials {
                shown.push_str(" (credentials withheld)");
            }
            shown
        }
        Err(_) => "<unparseable proxy value withheld>".to_string(),
    }
}

static STARTUP_OUTCOME: OnceLock<ProxyEnvOutcome> = OnceLock::new();

/// The startup step `main` runs before the Tokio runtime exists: read the
/// active profile's `network` block, export it into the process environment,
/// and record what was decided for the config report. Idempotent — a second
/// call returns the first outcome without writing anything.
///
/// Logging is not initialised yet when this runs, so it logs nothing;
/// [`log_startup_posture`] reports the recorded outcome once logging exists.
pub fn apply_profile_environment_at_startup() -> &'static ProxyEnvOutcome {
    STARTUP_OUTCOME.get_or_init(|| {
        let loaded = crate::profiles::network_with_source();
        let (network, profile) = match &loaded {
            Some((n, p)) => (Some(n), Some(p.as_str())),
            None => (None, None),
        };
        apply_profile_environment(network, profile, &mut ProcessEnv)
    })
}

/// The outcome recorded by [`apply_profile_environment_at_startup`] in THIS
/// process, or `None` when this process never ran it (the headless
/// `config_report` bin, every test binary).
pub fn startup_outcome() -> Option<&'static ProxyEnvOutcome> {
    STARTUP_OUTCOME.get()
}

/// Log the proxy posture once logging is initialised: which rung won, and —
/// on Windows — an explicit UNKNOWN when a PAC script is configured, because
/// PAC/WPAD is not evaluated and must never read as "no proxy".
pub fn log_startup_posture() {
    let Some(outcome) = startup_outcome() else {
        return;
    };
    info!(
        arm = outcome.arm.as_str(),
        proxy = outcome.proxy.as_deref().unwrap_or("-"),
        profile = outcome.profile.as_deref().unwrap_or("-"),
        no_proxy = %outcome.no_proxy,
        trust = ?outcome.trust,
        git_trust = outcome.git_trust.as_str(),
        ca_bundle = outcome.ca_bundle.as_deref().unwrap_or("-"),
        git_ssl_backend = outcome.git_ssl_backend.as_deref().unwrap_or("-"),
        "outbound network: proxy rung and TLS trust resolved"
    );
    if outcome.git_trust == GitTrustDecision::ConfigUnreadable {
        warn!(
            "outbound network: network.trust is \"os\" but `git config` could not be read at \
             startup (git missing, or the 5 s probe timed out before logging existed); git was \
             left on the machine's own TLS configuration"
        );
    }
    if pac_script_configured() == Some(true) && outcome.arm == ProxyEnvArm::None {
        warn!(
            "outbound network: UNKNOWN: a PAC script is configured; set network.proxy_url in \
             the profile — PAC/WPAD is not evaluated, so whether a proxy applies is not known"
        );
    }
}

/// Whether the Windows per-user Internet Settings name a PAC script
/// (`AutoConfigURL`). `None` off Windows or when the key is unreadable.
///
/// This reads ONE value to decide whether to say UNKNOWN; it is not a proxy
/// reader — the system proxy itself is read by `hyper-util`'s matcher.
pub fn pac_script_configured() -> Option<bool> {
    #[cfg(windows)]
    {
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        let key = hkcu
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
            .ok()?;
        let url: Result<String, _> = key.get_value("AutoConfigURL");
        Some(url.is_ok_and(|u| !u.trim().is_empty()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    const BUDGET: Duration = Duration::from_secs(10);

    // ---------------------------------------------------------------------
    // Harness: a recording CONNECT proxy and a WebSocket echo origin.
    // ---------------------------------------------------------------------

    #[derive(Clone, Copy)]
    enum ProxyMode {
        /// Answer 200 and splice to the requested port on 127.0.0.1.
        Tunnel,
        /// Answer 407.
        AuthRequired,
    }

    /// A CONNECT proxy that records every request head it receives. The
    /// tunnelled destination's PORT is honoured and its host is mapped to
    /// 127.0.0.1, so the target can be named `127.0.0.2` (not loopback-exempt)
    /// while the echo origin binds the one loopback address every OS has.
    async fn spawn_proxy(mode: ProxyMode) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_task = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let seen = seen_task.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut byte = [0u8; 1];
                    while !buf.ends_with(b"\r\n\r\n") {
                        if client.read(&mut byte).await.unwrap_or(0) == 0 {
                            return;
                        }
                        buf.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    seen.lock().unwrap().push(head.clone());
                    match mode {
                        ProxyMode::AuthRequired => {
                            let _ = client
                                .write_all(
                                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                                      Proxy-Authenticate: Basic realm=\"t\"\r\n\r\n",
                                )
                                .await;
                        }
                        ProxyMode::Tunnel => {
                            let target = head
                                .lines()
                                .next()
                                .and_then(|l| l.split_whitespace().nth(1))
                                .unwrap_or_default()
                                .to_string();
                            let port: u16 = target.rsplit(':').next().unwrap().parse().unwrap();
                            let mut upstream =
                                TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                            client
                                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                                .await
                                .unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                    }
                });
            }
        });
        (port, seen)
    }

    /// A WebSocket echo origin on 127.0.0.1.
    async fn spawn_echo() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        if msg.is_text() && ws.send(msg).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    /// An origin that refuses every upgrade 401.
    async fn spawn_unauthorized_origin() -> u16 {
        use tokio_tungstenite::tungstenite::handshake::server::{
            ErrorResponse, Request as SReq, Response as SResp,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = tokio_tungstenite::accept_hdr_async(
                        stream,
                        |_req: &SReq, _resp: SResp| -> Result<SResp, ErrorResponse> {
                            let mut err = ErrorResponse::new(Some("expired".into()));
                            *err.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
                            Err(err)
                        },
                    )
                    .await;
                });
            }
        });
        port
    }

    fn proxy_matcher(proxy_port: u16) -> Matcher {
        Matcher::builder()
            .http(format!("http://127.0.0.1:{proxy_port}"))
            .build()
    }

    fn request(url: &str) -> Request {
        url.into_client_request().unwrap()
    }

    async fn echo_round_trip(mut ws: CoordWs) {
        ws.send(Message::Text("ping-through".into())).await.unwrap();
        let back = tokio::time::timeout(BUDGET, ws.next())
            .await
            .expect("echo answered in time")
            .expect("stream open")
            .expect("frame ok");
        assert_eq!(back.into_text().unwrap().as_str(), "ping-through");
    }

    // ---------------------------------------------------------------------
    // Log capture (scoped to this test's thread).
    // ---------------------------------------------------------------------

    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
        type Writer = Sink;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    // ---------------------------------------------------------------------
    // The tunnel
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn a_ws_upgrade_is_tunnelled_through_a_connect_proxy() {
        let echo = spawn_echo().await;
        let (proxy, seen) = spawn_proxy(ProxyMode::Tunnel).await;
        let ws = connect_ws_with(
            request(&format!("ws://127.0.0.2:{echo}/ws?subscribe=device")),
            BUDGET,
            &proxy_matcher(proxy),
        )
        .await
        .expect("connect through the proxy");
        echo_round_trip(ws).await;
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen.len(),
            1,
            "exactly one CONNECT reached the proxy: {seen:?}"
        );
        assert!(
            seen[0].starts_with(&format!("CONNECT 127.0.0.2:{echo} HTTP/1.1\r\n")),
            "the proxy saw a CONNECT for the origin: {:?}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn a_407_from_the_proxy_is_proxy_auth_required() {
        let (proxy, _seen) = spawn_proxy(ProxyMode::AuthRequired).await;
        let err = connect_ws_with(
            request("ws://127.0.0.2:9/ws"),
            BUDGET,
            &proxy_matcher(proxy),
        )
        .await
        .expect_err("the proxy refused");
        assert!(
            matches!(err, WsConnectError::ProxyAuthRequired),
            "407 must surface as ProxyAuthRequired, got {err:?}"
        );
        assert!(
            !err.is_unauthorized(),
            "a proxy 407 must never kick the JWT refresher"
        );
    }

    #[tokio::test]
    async fn an_origin_401_through_the_tunnel_is_still_unauthorized() {
        let origin = spawn_unauthorized_origin().await;
        let (proxy, seen) = spawn_proxy(ProxyMode::Tunnel).await;
        let err = connect_ws_with(
            request(&format!("ws://127.0.0.2:{origin}/ws")),
            BUDGET,
            &proxy_matcher(proxy),
        )
        .await
        .expect_err("the origin refused");
        let inner = err
            .as_tungstenite()
            .expect("an origin refusal is tungstenite's");
        assert!(
            crate::coord_ws::upgrade_refusal_is_unauthorized(inner),
            "a 401 through the tunnel must still read as unauthorized, got {err:?}"
        );
        assert!(err.is_unauthorized());
        assert_eq!(seen.lock().unwrap().len(), 1, "it went through the proxy");
    }

    #[tokio::test]
    async fn no_proxy_and_loopback_go_direct() {
        let echo = spawn_echo().await;
        let (proxy, seen) = spawn_proxy(ProxyMode::Tunnel).await;
        // Loopback, with no NO_PROXY at all, connects direct.
        let ws = connect_ws_with(
            request(&format!("ws://127.0.0.1:{echo}/ws")),
            BUDGET,
            &proxy_matcher(proxy),
        )
        .await
        .expect("direct to loopback");
        echo_round_trip(ws).await;
        assert!(
            seen.lock().unwrap().is_empty(),
            "loopback must never reach the proxy"
        );

        // Routing decisions, without sockets.
        let m = Matcher::builder()
            .http(format!("http://127.0.0.1:{proxy}"))
            .https("http://secure-proxy.example.test:3128")
            .no("internal.example.test, 10.0.0.0/8")
            .build();
        let route = |u: &str| ProxyRoute::for_url(&u.parse::<Uri>().unwrap(), &m);
        for direct in [
            "ws://127.0.0.1:1/ws",
            "ws://localhost:1/ws",
            "ws://[::1]:1/ws",
            "wss://coord.internal.example.test/ws",
            "ws://10.1.2.3:8000/ws",
        ] {
            assert!(route(direct).is_none(), "{direct} must go direct");
        }
        let plain = route("ws://127.0.0.2:4000/ws").expect("ws:// uses the http proxy");
        assert_eq!(plain.proxy_authority(), format!("127.0.0.1:{proxy}"));
        assert_eq!(plain.target_authority(), "127.0.0.2:4000");
        let secure =
            route("wss://coord.example.test/ws?token=x").expect("wss:// uses the https proxy");
        assert_eq!(secure.proxy_authority(), "secure-proxy.example.test:3128");
        assert_eq!(secure.target_authority(), "coord.example.test:443");
    }

    #[test]
    fn a_tunnel_route_needs_a_proxy_for_its_scheme() {
        let m = Matcher::builder()
            .https("http://p.example.test:8080")
            .build();
        let route = |u: &str| ProxyRoute::for_url(&u.parse::<Uri>().unwrap(), &m);
        assert!(
            route("ws://origin.example.test/ws").is_none(),
            "no http proxy configured"
        );
        assert!(route("wss://origin.example.test/ws").is_some());
    }

    #[tokio::test]
    async fn proxy_userinfo_becomes_proxy_authorization_and_is_never_logged() {
        let echo = spawn_echo().await;
        let (proxy, seen) = spawn_proxy(ProxyMode::Tunnel).await;
        let matcher = Matcher::builder()
            .http(format!("http://alice:s3cret-pw@127.0.0.1:{proxy}"))
            .build();

        let sink = Sink::default();
        let buf = sink.0.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let ws = connect_ws_with(
            request(&format!("ws://127.0.0.2:{echo}/ws?token=device-jwt-SECRET")),
            BUDGET,
            &matcher,
        )
        .await
        .expect("connect through an authenticating proxy");
        drop(guard);
        echo_round_trip(ws).await;

        let head = seen.lock().unwrap()[0].clone();
        // base64("alice:s3cret-pw")
        assert!(
            head.contains("Proxy-Authorization: Basic YWxpY2U6czNjcmV0LXB3\r\n"),
            "userinfo must become Proxy-Authorization: {head:?}"
        );
        let logs = String::from_utf8_lossy(&buf.lock().unwrap()).into_owned();
        assert!(
            logs.contains(&format!("through HTTP proxy 127.0.0.1:{proxy}")),
            "the capture must see the tunnel line, or this test proves nothing: {logs:?}"
        );
        for secret in [
            "s3cret-pw",
            "YWxpY2U6czNjcmV0LXB3",
            "device-jwt-SECRET",
            "alice",
        ] {
            assert!(
                !logs.contains(secret),
                "{secret:?} leaked into the logs: {logs:?}"
            );
        }
        let debug = format!(
            "{:?}",
            ProxyRoute::for_url(&"ws://127.0.0.2:1/".parse().unwrap(), &matcher)
        );
        assert!(
            !debug.contains("YWxp"),
            "Debug must withhold the credential: {debug}"
        );
    }

    #[tokio::test]
    async fn the_whole_attempt_is_bounded_by_the_timeout() {
        // A "proxy" that accepts and never answers.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = listener.accept().await {
                held.push(s);
            }
        });
        let err = connect_ws_with(
            request("ws://127.0.0.2:9/ws"),
            Duration::from_millis(300),
            &proxy_matcher(port),
        )
        .await
        .expect_err("a silent proxy times out");
        match err {
            WsConnectError::Ws(tungstenite::Error::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::TimedOut)
            }
            other => panic!("expected Io(TimedOut), got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // apply_profile_environment
    // ---------------------------------------------------------------------

    #[derive(Default)]
    struct MapEnv(HashMap<String, String>);
    impl EnvAccess for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
        fn set(&mut self, key: &str, value: &str) {
            ENV_WRITES.with(|w| w.set(w.get() + 1));
            self.0.insert(key.to_string(), value.to_string());
        }
    }

    thread_local! {
        /// Writes made through any [`MapEnv`] on this test thread.
        static ENV_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn net(proxy: Option<&str>, no_proxy: Option<&str>) -> NetworkProfile {
        NetworkProfile {
            proxy_url: proxy.map(str::to_string),
            no_proxy: no_proxy.map(str::to_string),
            ..NetworkProfile::default()
        }
    }

    fn sys(windows: bool, bypass: Option<&str>, git: Option<bool>) -> SystemInputs<'static> {
        let probe: &'static dyn Fn() -> Option<bool> = match git {
            None => &|| None,
            Some(true) => &|| Some(true),
            Some(false) => &|| Some(false),
        };
        SystemInputs {
            windows,
            system_bypass: bypass.map(str::to_string),
            git_tls_configured: probe,
        }
    }

    fn apply(
        n: Option<&NetworkProfile>,
        env: &mut MapEnv,
        system: &SystemInputs<'_>,
    ) -> ProxyEnvOutcome {
        apply_profile_environment_with(n, n.map(|_| "dev"), env, system)
    }

    #[test]
    fn the_profile_proxy_is_exported_when_the_operator_set_none() {
        let mut env = MapEnv::default();
        let n = net(
            Some("http://bob:pw@proxy.example.test:3128"),
            Some(".corp.example.test"),
        );
        let out = apply(Some(&n), &mut env, &sys(false, None, None));
        assert_eq!(out.arm, ProxyEnvArm::Profile);
        assert_eq!(out.profile.as_deref(), Some("dev"));
        assert_eq!(
            env.get("HTTPS_PROXY").as_deref(),
            Some("http://bob:pw@proxy.example.test:3128")
        );
        assert_eq!(env.get("HTTP_PROXY"), env.get("HTTPS_PROXY"));
        assert_eq!(
            out.proxy.as_deref(),
            Some("http://proxy.example.test:3128 (credentials withheld)")
        );
        assert_eq!(out.no_proxy, ".corp.example.test,127.0.0.1,::1,localhost");
        assert_eq!(env.get("NO_PROXY"), env.get("no_proxy"));
        assert!(out.exported.contains(&"HTTPS_PROXY".to_string()));
    }

    #[test]
    fn an_operator_proxy_wins_and_is_left_untouched() {
        let mut env = MapEnv::default();
        env.set("https_proxy", "http://operator.example.test:8080");
        env.set("NO_PROXY", "a.example.test, localhost");
        let n = net(Some("http://profile.example.test:3128"), None);
        let out = apply(Some(&n), &mut env, &sys(false, None, None));
        assert_eq!(out.arm, ProxyEnvArm::OperatorEnv);
        assert_eq!(
            out.proxy.as_deref(),
            Some("http://operator.example.test:8080")
        );
        assert_eq!(
            env.get("HTTPS_PROXY"),
            None,
            "the profile proxy must not be exported"
        );
        assert_eq!(env.get("HTTP_PROXY"), None);
        // Existing entries kept, loopback appended once (localhost deduped).
        assert_eq!(out.no_proxy, "a.example.test,localhost,127.0.0.1,::1");
    }

    /// H1: with an environment proxy in force on Windows, the registry bypass
    /// list joins the exported NO_PROXY, so hosts IT exempted stay exempt.
    #[test]
    fn the_windows_bypass_list_joins_no_proxy_when_an_env_proxy_is_in_force() {
        let bypass = convert_proxy_override("*.intranet.example.test;10.*;<local>");
        assert_eq!(bypass, "intranet.example.test,10.*,<local>");
        let mut env = MapEnv::default();
        let n = net(Some("http://proxy.example.test:3128"), None);
        let out = apply(Some(&n), &mut env, &sys(true, Some(&bypass), None));
        assert_eq!(
            out.no_proxy,
            "intranet.example.test,10.*,<local>,127.0.0.1,::1,localhost"
        );
        let m = Matcher::builder()
            .https("http://proxy.example.test:3128")
            .no(out.no_proxy.as_str())
            .build();
        assert!(
            ProxyRoute::for_url(&"wss://wiki.intranet.example.test/ws".parse().unwrap(), &m)
                .is_none()
        );
        assert!(ProxyRoute::for_url(&"wss://coord.example.test/ws".parse().unwrap(), &m).is_some());
    }

    #[test]
    fn a_ca_bundle_goes_to_node_unless_the_operator_set_one() {
        let n = NetworkProfile {
            ca_bundle: Some(std::path::PathBuf::from("/corp/root.pem")),
            ..NetworkProfile::default()
        };
        let mut env = MapEnv::default();
        env.set("NODE_EXTRA_CA_CERTS", "/operator/chosen.pem");
        let out = apply(Some(&n), &mut env, &sys(false, None, None));
        assert_eq!(
            env.get("NODE_EXTRA_CA_CERTS").as_deref(),
            Some("/operator/chosen.pem")
        );
        assert_eq!(out.ca_bundle.as_deref(), Some("/corp/root.pem"));
        assert!(!out.exported.contains(&"NODE_EXTRA_CA_CERTS".to_string()));
        assert!(!out.exported.contains(&"SSL_CERT_FILE".to_string()));
    }

    fn trust_os() -> NetworkProfile {
        NetworkProfile {
            trust: Some(TrustMode::Os),
            ..NetworkProfile::default()
        }
    }

    #[test]
    fn explicit_trust_os_points_git_at_schannel_after_existing_entries() {
        let mut env = MapEnv::default();
        env.set("GIT_CONFIG_COUNT", "1");
        env.set("GIT_CONFIG_KEY_0", "core.autocrlf");
        env.set("GIT_CONFIG_VALUE_0", "false");
        let n = trust_os();
        let out = apply(Some(&n), &mut env, &sys(true, None, Some(false)));
        assert_eq!(out.git_trust, GitTrustDecision::Schannel);
        assert_eq!(out.git_ssl_backend.as_deref(), Some("schannel"));
        assert_eq!(env.get("GIT_CONFIG_COUNT").as_deref(), Some("2"));
        assert_eq!(
            env.get("GIT_CONFIG_KEY_0").as_deref(),
            Some("core.autocrlf")
        );
        assert_eq!(
            env.get("GIT_CONFIG_KEY_1").as_deref(),
            Some("http.sslBackend")
        );
        assert_eq!(env.get("GIT_CONFIG_VALUE_1").as_deref(), Some("schannel"));
        assert_eq!(out.trust, Some(TrustMode::Os));
    }

    /// M1: every arm in which git is left alone, and why.
    #[test]
    fn git_is_left_alone_unless_explicit_trust_os_meets_an_unconfigured_git() {
        let cases: Vec<(
            &str,
            Option<NetworkProfile>,
            SystemInputs<'static>,
            GitTrustDecision,
        )> = vec![
            (
                "not windows",
                Some(trust_os()),
                sys(false, None, Some(false)),
                GitTrustDecision::NotWindows,
            ),
            (
                "trust absent",
                None,
                sys(true, None, Some(false)),
                GitTrustDecision::NotRequested,
            ),
            (
                "bundled",
                Some(NetworkProfile {
                    trust: Some(TrustMode::Bundled),
                    ..NetworkProfile::default()
                }),
                sys(true, None, Some(false)),
                GitTrustDecision::Bundled,
            ),
            (
                "machine git chose",
                Some(trust_os()),
                sys(true, None, Some(true)),
                GitTrustDecision::OperatorConfigured,
            ),
            (
                "git unreadable",
                Some(trust_os()),
                sys(true, None, None),
                GitTrustDecision::ConfigUnreadable,
            ),
        ];
        for (label, n, system, want) in cases {
            let mut env = MapEnv::default();
            let out = apply(n.as_ref(), &mut env, &system);
            assert_eq!(out.git_trust, want, "{label}");
            assert_eq!(out.git_ssl_backend, None, "{label}");
            assert_eq!(env.get("GIT_CONFIG_COUNT"), None, "{label}");
        }

        let mut env = MapEnv::default();
        env.set("GIT_CONFIG_COUNT", "1");
        env.set("GIT_CONFIG_KEY_0", "http.sslbackend");
        env.set("GIT_CONFIG_VALUE_0", "openssl");
        let n = trust_os();
        let out = apply(Some(&n), &mut env, &sys(true, None, Some(false)));
        assert_eq!(out.git_trust, GitTrustDecision::OperatorConfigured);
        assert_eq!(env.get("GIT_CONFIG_COUNT").as_deref(), Some("1"));

        let mut env = MapEnv::default();
        env.set("GIT_CONFIG_COUNT", "two");
        let out = apply(Some(&n), &mut env, &sys(true, None, Some(false)));
        assert_eq!(out.git_trust, GitTrustDecision::UnreadableCount);
        assert_eq!(env.get("GIT_CONFIG_KEY_0"), None);
    }

    #[test]
    fn redaction_never_prints_userinfo() {
        assert_eq!(
            redact_proxy_url("http://u:p@h.example.test:1"),
            "http://h.example.test:1 (credentials withheld)"
        );
        assert_eq!(
            redact_proxy_url("h.example.test:3128"),
            "http://h.example.test:3128"
        );
        assert_eq!(
            redact_proxy_url("http://u@h.example.test"),
            "http://h.example.test (credentials withheld)"
        );
        assert!(!redact_proxy_url("http://[bad").contains("bad"));
    }

    // ---------------------------------------------------------------------
    // Review fixes (H1, H2, M1, M2, L1) — written failing-first.
    // ---------------------------------------------------------------------

    /// C1: with no environment proxy in force, NO_PROXY is still exported —
    /// as a STRICT SUPERSET of the Windows registry bypass list (so the hosts
    /// IT exempted stay exempt; H1) plus loopback. hyper-util has no implicit
    /// loopback bypass and `<local>` matches nothing, so without loopback here
    /// reqwest would send `http://127.0.0.1:*` to the system proxy.
    #[test]
    fn no_env_proxy_exports_the_bypass_list_and_loopback() {
        let mut env = MapEnv::default();
        let out = apply(
            None,
            &mut env,
            &sys(true, Some("intranet.example.test"), None),
        );
        assert_eq!(out.arm, ProxyEnvArm::None);
        let want = "intranet.example.test,127.0.0.1,::1,localhost";
        assert_eq!(env.get("NO_PROXY").as_deref(), Some(want));
        assert_eq!(env.get("no_proxy").as_deref(), Some(want));
        // The matcher reqwest builds (system proxy on, env NO_PROXY set, so the
        // registry list is not consulted) routes the runner's own API direct.
        let m = Matcher::builder()
            .http("http://system-proxy.example.test:8080")
            .https("http://system-proxy.example.test:8080")
            .no(want)
            .build();
        for direct in [
            "http://127.0.0.1:9876/health",
            "http://localhost:9876/",
            "https://wiki.intranet.example.test/",
        ] {
            assert!(m.intercept(&direct.parse().unwrap()).is_none(), "{direct}");
        }
        assert!(m
            .intercept(&"https://coord.example.test/".parse().unwrap())
            .is_some());
    }

    /// C1: the profile's `network.no_proxy` is honoured without a proxy too.
    #[test]
    fn profile_no_proxy_is_honoured_without_an_env_proxy() {
        let mut env = MapEnv::default();
        let n = net(None, Some(".corp.example.test"));
        let out = apply(Some(&n), &mut env, &sys(false, None, None));
        assert_eq!(out.no_proxy, ".corp.example.test,127.0.0.1,::1,localhost");
        assert_eq!(env.get("NO_PROXY").as_deref(), Some(out.no_proxy.as_str()));
    }

    /// W3: the git probe (which spawns a subprocess and reader threads) runs
    /// BEFORE any environment write, so every `set_var` happens after it.
    #[test]
    fn the_git_probe_runs_before_any_env_write() {
        static WRITES_AT_PROBE: std::sync::atomic::AtomicUsize =
            std::sync::atomic::AtomicUsize::new(usize::MAX);
        ENV_WRITES.with(|w| w.set(0));
        let probe: &dyn Fn() -> Option<bool> = &|| {
            WRITES_AT_PROBE.store(
                ENV_WRITES.with(|w| w.get()),
                std::sync::atomic::Ordering::SeqCst,
            );
            Some(false)
        };
        let n = NetworkProfile {
            proxy_url: Some("http://proxy.example.test:3128".into()),
            no_proxy: Some(".corp.example.test".into()),
            ca_bundle: Some(std::path::PathBuf::from("/corp/root.pem")),
            trust: Some(TrustMode::Os),
        };
        let mut env = MapEnv::default();
        let out = apply(
            Some(&n),
            &mut env,
            &SystemInputs {
                windows: true,
                system_bypass: None,
                git_tls_configured: probe,
            },
        );
        assert_eq!(out.git_trust, GitTrustDecision::Schannel);
        assert_eq!(
            WRITES_AT_PROBE.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the probe must run before the first env write"
        );
    }

    /// W2: a URL-scoped `http.<url>.sslCAInfo` / `.sslBackend` counts as the
    /// machine having chosen git's TLS trust.
    #[test]
    fn the_git_tls_key_pattern_matches_url_scoped_keys() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("gitconfig");
        let probe = |body: &str| -> Option<i32> {
            std::fs::write(&cfg, body).unwrap();
            let mut cmd = crate::process_helpers::scrubbed_git(std::ffi::OsStr::new("git"));
            cmd.arg("config")
                .arg("--file")
                .arg(&cfg)
                .args(["--get-regexp", GIT_TLS_KEYS_PATTERN]);
            crate::process_helpers::output_with_timeout(cmd, Duration::from_secs(20))
                .ok()
                .and_then(|o| o.status.code())
        };
        if probe("").is_none() {
            eprintln!("git not runnable here — pattern not exercised");
            return;
        }
        for (body, want) in [
            ("[http]\n\tsslBackend = openssl\n", 0),
            ("[http]\n\tsslCAInfo = /etc/corp.pem\n", 0),
            (
                "[http \"https://git.example.test/\"]\n\tsslCAInfo = /etc/corp.pem\n",
                0,
            ),
            (
                "[http \"https://git.example.test/\"]\n\tsslBackend = schannel\n",
                0,
            ),
            ("[http]\n\tsslVerify = true\n", 1),
            ("[core]\n\tautocrlf = false\n", 1),
        ] {
            assert_eq!(probe(body), Some(want), "{body:?}");
        }
    }

    /// L3: an operator's `NO_PROXY=*` (proxy nothing) is left alone.
    #[test]
    fn an_operator_no_proxy_star_is_left_alone() {
        let mut env = MapEnv::default();
        env.set("HTTPS_PROXY", "http://operator.example.test:8080");
        env.set("NO_PROXY", "*");
        let out = apply(None, &mut env, &sys(false, None, None));
        assert_eq!(env.get("NO_PROXY").as_deref(), Some("*"));
        assert_eq!(env.get("no_proxy"), None);
        assert_eq!(out.no_proxy, "*");
    }

    /// H2: the runner never sets SSL_CERT_FILE in its own process — on Linux
    /// that REPLACES the OS store for rustls-native-certs and OpenSSL. Only the
    /// additive NODE_EXTRA_CA_CERTS is exported.
    #[test]
    fn a_ca_bundle_never_replaces_the_os_store() {
        let mut env = MapEnv::default();
        let n = NetworkProfile {
            ca_bundle: Some(std::path::PathBuf::from("/corp/root.pem")),
            ..NetworkProfile::default()
        };
        apply(Some(&n), &mut env, &sys(false, None, None));
        assert_eq!(
            env.get("NODE_EXTRA_CA_CERTS").as_deref(),
            Some("/corp/root.pem")
        );
        assert_eq!(
            env.get("SSL_CERT_FILE"),
            None,
            "SSL_CERT_FILE replaces the OS store"
        );
    }

    /// M1: without `network.trust: os` explicitly in the profile, git on
    /// Windows is left on whatever the machine's git config chose.
    #[test]
    fn git_is_left_alone_unless_trust_os_is_explicit() {
        let mut env = MapEnv::default();
        let out = apply(None, &mut env, &sys(true, None, Some(false)));
        assert_eq!(out.git_ssl_backend, None);
        assert_eq!(env.get("GIT_CONFIG_COUNT"), None);
        let implicit = NetworkProfile {
            proxy_url: Some("http://p.example.test:1".into()),
            ..NetworkProfile::default()
        };
        let mut env = MapEnv::default();
        let out = apply(Some(&implicit), &mut env, &sys(true, None, Some(false)));
        assert_eq!(out.git_ssl_backend, None, "trust absent is not trust os");
    }

    /// L1: a profile's Debug output never carries the proxy credential.
    #[test]
    fn network_profile_debug_withholds_proxy_userinfo() {
        let n = NetworkProfile {
            proxy_url: Some("http://carol:hunter2-pw@proxy.example.test:3128".into()),
            ..NetworkProfile::default()
        };
        let shown = format!("{n:?}");
        assert!(!shown.contains("hunter2-pw"), "{shown}");
        assert!(!shown.contains("carol"), "{shown}");
        assert!(shown.contains("proxy.example.test:3128"), "{shown}");
    }

    /// M2: a proxy the WebSocket path cannot tunnel through (socks5, an
    /// https:// proxy) is skipped — the socket goes direct, as before this
    /// module existed — instead of failing every connect.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn an_unsupported_proxy_scheme_goes_direct() {
        // Linux routes all of 127/8 to lo, so an origin can bind 127.0.0.2
        // (which is not loopback-exempt) and be reached directly.
        let listener = TcpListener::bind("127.0.0.2:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                        while let Some(Ok(msg)) = ws.next().await {
                            if msg.is_text() && ws.send(msg).await.is_err() {
                                return;
                            }
                        }
                    }
                });
            }
        });
        for proxy in ["socks5://127.0.0.1:1", "https://127.0.0.1:1"] {
            let matcher = Matcher::builder().all(proxy).build();
            let ws = connect_ws_with(
                request(&format!("ws://127.0.0.2:{port}/ws")),
                BUDGET,
                &matcher,
            )
            .await
            .unwrap_or_else(|e| panic!("{proxy}: must go direct, got {e:?}"));
            echo_round_trip(ws).await;
        }
    }

    // ---------------------------------------------------------------------
    // The Windows system-proxy arm (Phase 4 step 4).
    // ---------------------------------------------------------------------

    /// A registry-only proxy (WinINet `ProxyEnable` / `ProxyServer`, no proxy
    /// env var) is returned by `Matcher::from_system()` — so the WebSocket path
    /// follows the Windows system proxy with no registry code of its own.
    ///
    /// It WRITES `HKCU\...\Internet Settings` (restoring it afterwards), so it
    /// runs only on a GitHub Actions runner, never on a developer's box where
    /// it would flip the user's live system proxy for its duration.
    #[cfg(windows)]
    #[test]
    fn windows_registry_only_proxy_is_seen_by_the_system_matcher() {
        if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") {
            eprintln!("skipped: writes the user's Internet Settings; runs on CI only");
            return;
        }
        for k in OPERATOR_PROXY_VARS.iter().chain(&["NO_PROXY", "no_proxy"]) {
            assert!(
                std::env::var(k).is_err(),
                "{k} is set on this CI host; the registry arm cannot be isolated"
            );
        }
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings",
                KEY_READ | KEY_WRITE,
            )
            .unwrap();

        /// Restores the three values on drop — including when an assertion
        /// or an unwrap below panics.
        struct Restore {
            key: winreg::RegKey,
            enable: Option<u32>,
            server: Option<String>,
            overrides: Option<String>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = match self.enable {
                    Some(v) => self.key.set_value("ProxyEnable", &v),
                    None => self.key.delete_value("ProxyEnable"),
                };
                let _ = match &self.server {
                    Some(v) => self.key.set_value("ProxyServer", v),
                    None => self.key.delete_value("ProxyServer"),
                };
                let _ = match &self.overrides {
                    Some(v) => self.key.set_value("ProxyOverride", v),
                    None => self.key.delete_value("ProxyOverride"),
                };
            }
        }
        let guard = Restore {
            enable: key.get_value("ProxyEnable").ok(),
            server: key.get_value("ProxyServer").ok(),
            overrides: key.get_value("ProxyOverride").ok(),
            key,
        };
        let key = &guard.key;
        key.set_value("ProxyEnable", &1u32).unwrap();
        key.set_value("ProxyServer", &"registry-proxy.example.test:8123")
            .unwrap();
        key.set_value("ProxyOverride", &"*.intranet.example.test;<local>")
            .unwrap();

        let system = Matcher::from_system();
        let seen = ProxyRoute::for_url(&"wss://coord.example.test/ws".parse().unwrap(), &system)
            .map(|r| r.proxy_authority());
        let intranet_direct_system = ProxyRoute::for_url(
            &"wss://wiki.intranet.example.test/ws".parse().unwrap(),
            &system,
        )
        .is_none();
        let bypass = windows_system_bypass();
        // H1: with an env proxy in force, the exported NO_PROXY keeps the
        // registry bypass list, so the intranet host stays direct.
        let mut env = MapEnv::default();
        let n = net(Some("http://env-proxy.example.test:3128"), None);
        let out = apply(
            Some(&n),
            &mut env,
            &SystemInputs {
                windows: true,
                system_bypass: bypass.clone(),
                git_tls_configured: &|| Some(true),
            },
        );
        let union = Matcher::builder()
            .https("http://env-proxy.example.test:3128")
            .no(out.no_proxy.as_str())
            .build();
        let intranet_direct_union = ProxyRoute::for_url(
            &"wss://wiki.intranet.example.test/ws".parse().unwrap(),
            &union,
        )
        .is_none();

        drop(guard);
        assert_eq!(seen.as_deref(), Some("registry-proxy.example.test:8123"));
        assert!(
            intranet_direct_system,
            "the registry bypass list exempts the intranet"
        );
        assert_eq!(bypass.as_deref(), Some("intranet.example.test,<local>"));
        assert!(
            intranet_direct_union,
            "an env proxy must not lose the registry bypass list: {}",
            out.no_proxy
        );
    }

    // ---------------------------------------------------------------------
    // The source scan (decision C1).
    // ---------------------------------------------------------------------

    /// Test-only module FILES (declared `#[cfg(test)] mod tests;`) that may
    /// call `connect_async(` directly. Named one by one, and each one's
    /// declaration is checked, so a production file cannot hide here.
    const TEST_ONLY_FILES: &[(&str, &str)] =
        &[("mcp/relay_binding/tests.rs", "mcp/relay_binding.rs")];

    /// Byte ranges of `#[cfg(test)] mod <name> { … }` blocks in `src`.
    fn cfg_test_ranges(src: &str) -> Vec<(usize, usize)> {
        let mut ranges = Vec::new();
        let mut from = 0;
        while let Some(off) = src[from..].find("#[cfg(test)]") {
            let attr = from + off;
            let after = attr + "#[cfg(test)]".len();
            from = after;
            let rest = src[after..].trim_start();
            if !(rest.starts_with("mod ")
                || rest.starts_with("pub mod ")
                || rest.starts_with("pub(crate) mod "))
            {
                continue;
            }
            let Some(brace_rel) = src[after..].find(['{', ';']) else {
                continue;
            };
            let brace = after + brace_rel;
            if src.as_bytes()[brace] == b';' {
                continue;
            }
            let mut depth = 0usize;
            let mut end = src.len();
            for (i, c) in src[brace..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = brace + i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            ranges.push((attr, end));
        }
        ranges
    }

    #[test]
    fn no_connect_async_outside_outbound_net() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for (file, decl) in TEST_ONLY_FILES {
            let parent = std::fs::read_to_string(root.join(decl)).unwrap();
            assert!(
                parent.contains("#[cfg(test)]\nmod tests;"),
                "{file} is exempt only because {decl} declares it `#[cfg(test)] mod tests;`"
            );
        }
        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let rel = path
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel == "outbound_net.rs" || TEST_ONLY_FILES.iter().any(|(f, _)| *f == rel) {
                    continue;
                }
                scanned += 1;
                let src = std::fs::read_to_string(&path).unwrap();
                for line in direct_ws_connects(&src) {
                    offenders.push(format!("{rel}:{line}"));
                }
            }
        }
        assert!(
            scanned > 100,
            "the scan must actually walk src/ (saw {scanned} files)"
        );
        assert!(
            offenders.is_empty(),
            "every WebSocket connect goes through outbound_net::connect_ws (proxy-aware); \
             found direct connect_async( at: {offenders:?}"
        );
    }

    /// Every spelling of a direct tungstenite connect: the plain and config
    /// variants, the TLS variants, and the bring-your-own-stream `client_async*`
    /// family.
    const DIRECT_WS_CONNECTS: &[&str] = &[
        "connect_async(",
        "connect_async_with_config(",
        "connect_async_tls_with_config(",
        "client_async(",
        "client_async_with_config(",
        "client_async_tls(",
        "client_async_tls_with_config(",
    ];

    /// Line numbers of production (non-comment, non-`#[cfg(test)]`) direct
    /// WebSocket connects in `src`.
    fn direct_ws_connects(src: &str) -> Vec<usize> {
        let exempt = cfg_test_ranges(src);
        let mut lines = Vec::new();
        for pat in DIRECT_WS_CONNECTS {
            let mut from = 0;
            while let Some(off) = src[from..].find(pat) {
                let at = from + off;
                from = at + 1;
                // `connect_async(` must not match inside `…_connect_async(`
                // of a longer identifier, nor re-count a longer pattern.
                if src[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    continue;
                }
                let line_start = src[..at].rfind('\n').map_or(0, |i| i + 1);
                if src[line_start..at].trim_start().starts_with("//") {
                    continue;
                }
                if exempt.iter().any(|(s, e)| at > *s && at < *e) {
                    continue;
                }
                lines.push(src[..at].matches('\n').count() + 1);
            }
        }
        lines.sort_unstable();
        lines
    }

    #[test]
    fn the_scanner_sees_every_direct_connect_spelling() {
        for (i, pat) in DIRECT_WS_CONNECTS.iter().enumerate() {
            let src = format!("fn f() {{ tokio_tungstenite::{pat}r, s); }}\n");
            assert_eq!(direct_ws_connects(&src), vec![1], "pattern {i}: {pat}");
        }
        assert!(direct_ws_connects("fn f() { my_connect_async(x); }\n").is_empty());
    }

    #[test]
    fn the_scanner_sees_a_production_call_and_exempts_a_test_module() {
        let src = "fn a() { tokio_tungstenite::connect_async(r); }\n\
                   #[cfg(test)]\nmod tests {\n fn b() { connect_async(x); }\n}\n";
        let ranges = cfg_test_ranges(src);
        let first = src.find("connect_async(").unwrap();
        let second = src.rfind("connect_async(").unwrap();
        assert!(!ranges.iter().any(|(s, e)| first > *s && first < *e));
        assert!(ranges.iter().any(|(s, e)| second > *s && second < *e));
    }
}
