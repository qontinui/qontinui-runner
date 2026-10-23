//! The `svc:imagegen` capability: this device can serve OpenAI-images work
//! from a LOCAL endpoint (plan `2026-10-13-image-generation-and-visual-judgment`,
//! Phase 25 step 1).
//!
//! # What is probed
//!
//! `GET <endpoint>/v1/models` on the local llama-swap, the one GPU owner the
//! image facade sits behind (that plan's Phase 10, qontinui
//! `docker/llama-swap/config.yaml`, entry `qontinui-imagegen`). The token is
//! advertised iff that list carries at least one entry whose DECLARED output is
//! image generation. It is never inferred from an alias name: an alias list in
//! this file would advertise a host that merely names `z-image-turbo` for some
//! other purpose, and would miss every recipe added after this was written.
//!
//! # The declared marker, and why it is not `capabilities`
//!
//! llama-swap's per-model `capabilities.out: [image]` would be the natural
//! marker, but the version Phase 10 pins (v217) has no per-model
//! `capabilities`. Phase 10 therefore declares the entry with
//! `metadata: {qontinui_capability: image_generation}`, and llama-swap v217's
//! `listModelsHandler` (`proxy/proxymanager.go`) serves a model's metadata map
//! under `meta.llamaswap`:
//!
//! ```json
//! {"object":"list","data":[
//!   {"id":"qontinui-imagegen","object":"model","owned_by":"llama-swap",
//!    "meta":{"llamaswap":{"qontinui_capability":"image_generation"}}}]}
//! ```
//!
//! That nested `meta.llamaswap.qontinui_capability == "image_generation"` is
//! the only shape accepted. `/v1/models` lists model IDs, not aliases
//! (`includeAliasesInList` defaults to false), so the entry found is
//! `qontinui-imagegen`, not `z-image-turbo` — which is why the marker, not the
//! id, is what is matched.
//!
//! # Which endpoint
//!
//! [`ENV_IMAGEGEN_ENDPOINT`] (`QONTINUI_IMAGEGEN_ENDPOINT`) when set and
//! non-blank, else [`DEFAULT_IMAGEGEN_ENDPOINT`], `http://127.0.0.1:8100`: the
//! port Phase 10's compose file publishes llama-swap on, and the same default
//! `mcp::ui_bridge::vision_ai` and `verification::mode` use for that llama-swap.
//! A base URL, like theirs: `/v1/models` is appended, and a trailing `/` is
//! dropped first. Loopback by default on purpose — the plan's D1.6 keeps the
//! image door loopback-only, and discovery is by this token, never by address.
//!
//! # Fail-closed, bounded, cached
//!
//! * Every failure — a refused or timed-out connection, a non-2xx status, an
//!   oversized or malformed body, no marked entry — advertises NOTHING. The
//!   token claims "image work placed here can be served"; over-advertising it
//!   is the failure with teeth (a placed asset session that cannot generate),
//!   under-advertising only places the work elsewhere.
//! * The probe runs on the blocking pool (`heartbeat_to_coord` runs
//!   `host_capabilities` through `spawn_blocking_tracked`), never on an async
//!   worker, and its network part is bounded. reqwest's blocking `timeout`
//!   bounds the send and EACH body `read()` separately, so on its own a server
//!   trickling one byte just inside every timeout could hold the probe for as
//!   long as it takes to fill the body cap. So once the client is built, the
//!   probe takes a deadline of [`REQUEST_TIMEOUT`], reads the body in its own
//!   loop, and refuses a declared `Content-Length` over the cap before reading
//!   anything. The deadline is checked AFTER each read, never before one, so a
//!   body that has already arrived is never thrown away: a read that ends past
//!   the deadline keeps the body if it is complete (its declared length is
//!   met, or it parses as a whole JSON document) and gives up otherwise.
//! * **What is bounded, exactly.** From the moment the client is built, a probe
//!   costs less than 2 × [`REQUEST_TIMEOUT`] (4 s): the send, connect
//!   ([`CONNECT_TIMEOUT`]) and name resolution included, plus at most one read
//!   that started before the deadline. That holds for a hostname endpoint as
//!   well as an IP literal, because names resolve through
//!   `outbound_net::os_dns` (`getaddrinfo` on its own detached thread, capped at
//!   `os_dns::LOOKUP_TIMEOUT`), not reqwest's default resolver, whose lookup on
//!   the client's blocking pool a dropped client would wait for. BUILDING the
//!   client is not bounded: `reqwest::blocking::Client::build` starts the
//!   client's runtime thread and sets up TLS and waits for both with no
//!   timeout. That is why the deadline starts after the build, so a slow build
//!   on a loaded box cannot spend the deadline of a healthy response.
//! * Verdicts are cached ASYMMETRICALLY, because over-advertising is the costly
//!   direction. A `true` is cached for [`POSITIVE_VERDICT_TTL`] (25 s), under
//!   one default heartbeat interval (30 s), so a positive verdict is re-probed
//!   on every tick (sub-millisecond on loopback), and a stopped image upstream
//!   is advertised at most [`POSITIVE_VERDICT_TTL`] after the last successful
//!   probe started (then the next heartbeat carries the withdrawal). A `false` is
//!   cached for the parent's `HOST_PROBE_TTL` (300 s), like the PATH and
//!   Docker probes, so a host with no image upstream pays one probe per TTL,
//!   and a newly started one is advertised within that TTL.
//!
//! # What the token does NOT claim
//!
//! It reads llama-swap's model LIST, which answers whether or not the facade
//! itself would serve: a facade switched off (`QONTINUI_IMAGEGEN=0`), one
//! refusing `listener_not_attested`, or one whose weights are not installed
//! still has its entry listed. Those answer a typed refusal at request time;
//! this token says only that the image upstream is DECLARED on this host.

use std::io::Read;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::debug;

/// The capability token. Spelling is a wire contract: a placement naming it in
/// `required_capabilities` matches by exact string (coord
/// `agent_placement.rs`), so a respelling silently un-targets every request.
pub(crate) const IMAGEGEN_CAPABILITY: &str = "svc:imagegen";

/// Environment override for the probed endpoint's base URL.
pub(crate) const ENV_IMAGEGEN_ENDPOINT: &str = "QONTINUI_IMAGEGEN_ENDPOINT";

/// Default base URL: the local llama-swap.
pub(crate) const DEFAULT_IMAGEGEN_ENDPOINT: &str = "http://127.0.0.1:8100";

/// The `meta.llamaswap` key and value Phase 10's `config.yaml` declares on the
/// image entry.
const IMAGE_MARKER_KEY: &str = "qontinui_capability";
const IMAGE_MARKER_VALUE: &str = "image_generation";

/// TCP connect budget. Loopback answers in well under a millisecond; this
/// bounds a configured remote host that drops SYNs.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// The probe's deadline, taken once the client is built, and reqwest's
/// per-operation timeout (the send, connect and resolution included, and each
/// body read). `/v1/models` is served from llama-swap's config without
/// touching any model, so it never waits on a swap or a load. The wall-clock
/// bound this buys, and what it leaves out, is in the module docs: less than
/// twice this value after the client is built.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a POSITIVE verdict is cached: under one default heartbeat interval
/// (`COORD_HEARTBEAT_INTERVAL_SECS`, 30 s), so every tick re-probes a host
/// that advertised the token, and a stopped image upstream is advertised at
/// most this long after the last successful probe started. A NEGATIVE verdict
/// is cached for the parent's
/// `HOST_PROBE_TTL`. See the module docs for why the two differ.
pub(crate) const POSITIVE_VERDICT_TTL: Duration = Duration::from_secs(25);

/// A model list larger than this is not a llama-swap answer; refuse it rather
/// than buffer it.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

/// The cached verdict and when it was taken.
static IMAGEGEN_PROBE_CACHE: Mutex<Option<(bool, Instant)>> = Mutex::new(None);

/// The heartbeat's `svc:imagegen` contribution: the token, or nothing.
/// A `true` is cached for [`POSITIVE_VERDICT_TTL`], a `false` for
/// `HOST_PROBE_TTL`. BLOCKS on a cache miss (one bounded HTTP request), so
/// call it only off the async workers.
pub(crate) fn capability_tokens() -> Vec<String> {
    #[cfg(test)]
    if let Some(forced) = FORCED_VERDICT.with(std::cell::Cell::get) {
        return tokens_for(forced);
    }
    let available = cached_probe(
        &IMAGEGEN_PROBE_CACHE,
        Instant::now(),
        POSITIVE_VERDICT_TTL,
        super::HOST_PROBE_TTL,
        || {
            let endpoint = resolve_endpoint(std::env::var(ENV_IMAGEGEN_ENDPOINT).ok().as_deref());
            probe_endpoint(&endpoint, CONNECT_TIMEOUT, REQUEST_TIMEOUT)
        },
    );
    tokens_for(available)
}

#[cfg(test)]
thread_local! {
    /// Test seam: a verdict that replaces the probe on THIS thread only, so a
    /// test of the heartbeat's assembly neither depends on whether this host
    /// runs an image upstream nor races another test thread's real probe
    /// writing the shared cache.
    static FORCED_VERDICT: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// Test seam: how long a client build takes on THIS thread, slept right
    /// after the real build, so a test can stand in for a loaded box whose
    /// `Client::build` is slow.
    static TEST_BUILD_DELAY: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// Force (`Some`) or release (`None`) this thread's verdict. Test-only.
#[cfg(test)]
pub(crate) fn force_verdict_for_test(verdict: Option<bool>) {
    FORCED_VERDICT.with(|f| f.set(verdict));
}

/// The token set for a settled verdict. Pure.
fn tokens_for(available: bool) -> Vec<String> {
    if available {
        vec![IMAGEGEN_CAPABILITY.to_string()]
    } else {
        Vec::new()
    }
}

/// Returns the cached verdict while it is younger, at `now`, than ITS OWN
/// TTL (`positive_ttl` for a `true`, `negative_ttl` for a `false`);
/// otherwise runs `probe` and caches its verdict as of `now`. A poisoned lock
/// degrades to probing uncached, never to a panic.
fn cached_probe(
    cache: &Mutex<Option<(bool, Instant)>>,
    now: Instant,
    positive_ttl: Duration,
    negative_ttl: Duration,
    probe: impl FnOnce() -> bool,
) -> bool {
    if let Ok(g) = cache.lock() {
        if let Some((verdict, taken)) = g.as_ref() {
            let ttl = if *verdict { positive_ttl } else { negative_ttl };
            if now.saturating_duration_since(*taken) < ttl {
                return *verdict;
            }
        }
    }
    let verdict = probe();
    if let Ok(mut g) = cache.lock() {
        *g = Some((verdict, now));
    }
    verdict
}

/// The base URL to probe: the override when set and non-blank, else the
/// default; trailing `/` dropped. Pure.
fn resolve_endpoint(raw: Option<&str>) -> String {
    let chosen = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_IMAGEGEN_ENDPOINT);
    chosen.trim_end_matches('/').to_string()
}

/// `GET <base>/v1/models` and decide. Every failure is `false`. `timeout` is
/// both reqwest's per-operation timeout and the deadline the body loop checks,
/// so once the client is built the call returns in less than 2 × `timeout`
/// (see the module docs for what that bound leaves out).
fn probe_endpoint(base: &str, connect_timeout: Duration, timeout: Duration) -> bool {
    let url = format!("{base}/v1/models");
    let client = match reqwest::blocking::Client::builder()
        // Resolve through the OS on os_dns's detached threads: dropping this
        // client then never waits on a lookup still running (see module docs).
        .dns_resolver(qontinui_runner_lib::outbound_net::os_dns::os_resolver())
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        // A local service: an operator's HTTP(S)_PROXY must not intercept it.
        .no_proxy()
        // A redirect is not a model list; never follow one off the host.
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: client build failed ({e}) — not advertised");
            return false;
        }
    };
    #[cfg(test)]
    std::thread::sleep(TEST_BUILD_DELAY.with(std::cell::Cell::get));
    // Taken AFTER the build, which has no timeout of its own: a slow build on a
    // loaded box must not spend the deadline of the request that follows it.
    let deadline = Instant::now() + timeout;
    let mut resp = match client.get(&url).send() {
        Ok(r) => r,
        Err(e) => {
            // debug!: on a host with no image upstream this is a standing,
            // correct property, and the heartbeat asks once per TTL.
            debug!(
                "host_capabilities: {IMAGEGEN_CAPABILITY}: GET {url} failed ({e}) — not advertised"
            );
            return false;
        }
    };
    let status = resp.status();
    if !status.is_success() {
        debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: GET {url} answered {status} — not advertised");
        return false;
    }
    let declared_len = resp.content_length();
    if declared_len.is_some_and(|n| n > MAX_BODY_BYTES) {
        debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} declares a body over {MAX_BODY_BYTES} bytes — not advertised");
        return false;
    }
    let Some(body) = read_body_by_deadline(&mut resp, declared_len, deadline, &url) else {
        return false;
    };
    match image_model_ids(&body) {
        Some(ids) if !ids.is_empty() => {
            debug!(
                "host_capabilities: {IMAGEGEN_CAPABILITY}: {url} declares image output on {ids:?}"
            );
            true
        }
        Some(_) => {
            debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} lists no image-output model — not advertised");
            false
        }
        None => {
            debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} answered no OpenAI model list — not advertised");
            false
        }
    }
}

/// Read the whole body, giving up (`None`) when the body grows past
/// [`MAX_BODY_BYTES`], on a read error, or when a read ends past `deadline`
/// with the body still incomplete. reqwest bounds each read on its own, not
/// the sum of them, so this loop is what bounds a trickling server.
///
/// The deadline is checked AFTER each read, never before one, so bytes that
/// have already arrived are never thrown away: a read ending past the deadline
/// keeps the body when it is already complete ([`body_is_complete`]). A new
/// read starts only while the deadline has not passed.
fn read_body_by_deadline(
    resp: &mut impl Read,
    declared_len: Option<u64>,
    deadline: Instant,
    url: &str,
) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match resp.read(&mut chunk) {
            Ok(0) => return Some(body),
            Ok(n) => {
                body.extend_from_slice(chunk.get(..n).unwrap_or_default());
                if body.len() as u64 > MAX_BODY_BYTES {
                    debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} body exceeds {MAX_BODY_BYTES} bytes — not advertised");
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => {
                debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: reading {url} failed ({e}) — not advertised");
                return None;
            }
        }
        if Instant::now() >= deadline {
            if body_is_complete(&body, declared_len) {
                return Some(body);
            }
            debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} body not complete by the deadline — not advertised");
            return None;
        }
    }
}

/// True when `body` is already the whole response: its declared length is met,
/// or it parses as one complete JSON document. A model list is a JSON object,
/// and no proper prefix of a JSON object parses, so a body that parses is not a
/// truncated one. Pure.
fn body_is_complete(body: &[u8], declared_len: Option<u64>) -> bool {
    declared_len.is_some_and(|n| body.len() as u64 >= n)
        || serde_json::from_slice::<serde::de::IgnoredAny>(body).is_ok()
}

/// The ids of the `/v1/models` entries that declare image output. `None` when
/// the body is not an OpenAI model list (`{"data": [ ... ]}`) at all; an entry
/// that is not an object with a string `id` is skipped, never counted. Pure.
fn image_model_ids(body: &[u8]) -> Option<Vec<String>> {
    let doc: serde_json::Value = serde_json::from_slice(body).ok()?;
    let data = doc.get("data")?.as_array()?;
    Some(
        data.iter()
            .filter(|entry| declares_image_output(entry))
            .filter_map(|entry| entry.get("id")?.as_str().map(str::to_string))
            .collect(),
    )
}

/// `meta.llamaswap.qontinui_capability == "image_generation"`, exactly.
fn declares_image_output(entry: &serde_json::Value) -> bool {
    entry
        .get("meta")
        .and_then(|m| m.get("llamaswap"))
        .and_then(|l| l.get(IMAGE_MARKER_KEY))
        .and_then(|v| v.as_str())
        == Some(IMAGE_MARKER_VALUE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// What llama-swap v217 serves for Phase 10's config: unmarked models
    /// beside the one marked image entry.
    const LLAMA_SWAP_WITH_IMAGE: &str = r#"{"object":"list","data":[
        {"id":"paddleocr","object":"model","created":1,"owned_by":"llama-swap"},
        {"id":"qontinui-grounding-v1","object":"model","created":1,"owned_by":"llama-swap"},
        {"id":"qontinui-imagegen","object":"model","created":1,"owned_by":"llama-swap",
         "meta":{"llamaswap":{"qontinui_capability":"image_generation"}}}]}"#;

    const LLAMA_SWAP_WITHOUT_IMAGE: &str = r#"{"object":"list","data":[
        {"id":"paddleocr","object":"model","created":1,"owned_by":"llama-swap"},
        {"id":"gemma-4-26b-a4b","object":"model","created":1,"owned_by":"llama-swap",
         "meta":{"llamaswap":{"qontinui_capability":"text_generation"}}}]}"#;

    /// Serve ONE HTTP request on a loopback port with `status` and `body`,
    /// returning the base URL.
    fn serve_once(status: &'static str, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn probe(base: &str) -> bool {
        probe_endpoint(
            base,
            Duration::from_millis(500),
            Duration::from_millis(1500),
        )
    }

    #[test]
    fn image_alias_present_advertises_the_token() {
        let base = serve_once("200 OK", LLAMA_SWAP_WITH_IMAGE.to_string());
        let available = probe(&base);
        assert!(available, "a marked image entry must advertise");
        assert_eq!(tokens_for(available), vec!["svc:imagegen"]);
    }

    #[test]
    fn no_image_alias_advertises_nothing() {
        let base = serve_once("200 OK", LLAMA_SWAP_WITHOUT_IMAGE.to_string());
        assert!(
            !probe(&base),
            "a list with no image-output entry must not advertise"
        );
        let empty = serve_once("200 OK", r#"{"object":"list","data":[]}"#.to_string());
        assert!(!probe(&empty), "an empty model list must not advertise");
    }

    /// An alias NAME is not a declaration: entries named like the image aliases
    /// but carrying no `meta.llamaswap` marker (an unrelated config, or the
    /// marker key in the wrong place) advertise nothing; there is no hard-coded
    /// alias list.
    #[test]
    fn an_image_alias_name_without_the_marker_advertises_nothing() {
        let body = r#"{"object":"list","data":[
            {"id":"z-image-turbo","object":"model","owned_by":"llama-swap"},
            {"id":"qwen-image-2512","object":"model","owned_by":"llama-swap"},
            {"id":"qontinui-imagegen","object":"model","owned_by":"llama-swap",
             "qontinui_capability":"image_generation"}]}"#;
        assert_eq!(image_model_ids(body.as_bytes()), Some(Vec::new()));
        assert!(!probe(&serve_once("200 OK", body.to_string())));
    }

    #[test]
    fn endpoint_down_advertises_nothing() {
        // Bind then drop: the port is (almost certainly) closed, so the connect
        // is refused.
        let port = TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        assert!(!probe(&format!("http://127.0.0.1:{port}")));
    }

    #[test]
    fn endpoint_timeout_advertises_nothing_within_the_budget() {
        // Accepts, reads, never answers.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                std::thread::sleep(Duration::from_secs(10));
            }
        });
        let started = Instant::now();
        let ok = probe_endpoint(
            &format!("http://{addr}"),
            Duration::from_millis(300),
            Duration::from_millis(600),
        );
        assert!(!ok, "a silent endpoint must not advertise");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the probe must be bounded by its timeout, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn malformed_json_advertises_nothing() {
        for body in [
            "not json at all",
            r#"{"object":"list","data":"qontinui-imagegen"}"#,
            r#"{"object":"list"}"#,
            r#"{"object":"list","data":[{"id":"qontinui-imagegen""#,
        ] {
            assert_eq!(image_model_ids(body.as_bytes()), None, "{body}");
            assert!(!probe(&serve_once("200 OK", body.to_string())), "{body}");
        }
    }

    #[test]
    fn non_success_status_advertises_nothing_even_with_a_marked_body() {
        let base = serve_once("503 Service Unavailable", LLAMA_SWAP_WITH_IMAGE.to_string());
        assert!(!probe(&base));
    }

    /// Serve ONE response whose head is written verbatim, then `body_chunks`
    /// one at a time with `gap` between them, stopping when the client goes.
    /// Then hold the connection for `hold`. Returns the base URL.
    fn serve_raw(head: String, body_chunks: Vec<Vec<u8>>, gap: Duration, hold: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for chunk in body_chunks {
                    std::thread::sleep(gap);
                    if stream
                        .write_all(&chunk)
                        .and_then(|_| stream.flush())
                        .is_err()
                    {
                        return;
                    }
                }
                std::thread::sleep(hold);
            }
        });
        format!("http://{addr}")
    }

    /// An oversized body with NO declared length (read to EOF) is caught by
    /// the read loop's cap, never parsed.
    #[test]
    fn oversized_body_advertises_nothing() {
        let pad = " ".repeat(MAX_BODY_BYTES as usize);
        let body = format!("{LLAMA_SWAP_WITH_IMAGE}{pad}").into_bytes();
        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                    Connection: close\r\n\r\n"
            .to_string();
        let base = serve_raw(head, vec![body], Duration::ZERO, Duration::ZERO);
        assert!(!probe(&base));
    }

    /// A declared `Content-Length` over the cap is refused BEFORE any body
    /// read: the server here sends no body at all and holds the connection,
    /// so only the early refusal returns well inside the per-read timeout.
    #[test]
    fn a_declared_oversized_length_is_refused_before_reading() {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let base = serve_raw(head, Vec::new(), Duration::ZERO, Duration::from_secs(10));
        let started = Instant::now();
        let ok = probe_endpoint(&base, Duration::from_millis(500), Duration::from_secs(3));
        assert!(!ok, "a body declared over the cap must not advertise");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the refusal must come before any read, took {:?}",
            started.elapsed()
        );
    }

    /// Serve ONE response: the head at once, then each `(at, bytes)` chunk at
    /// `at` after the connection was accepted, then hold the connection open
    /// for `hold` before closing it. Returns the base URL.
    fn serve_scheduled(head: String, chunks: Vec<(Duration, Vec<u8>)>, hold: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let accepted = Instant::now();
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                if stream
                    .write_all(head.as_bytes())
                    .and_then(|_| stream.flush())
                    .is_err()
                {
                    return;
                }
                for (at, bytes) in chunks {
                    std::thread::sleep(at.saturating_sub(accepted.elapsed()));
                    if stream
                        .write_all(&bytes)
                        .and_then(|_| stream.flush())
                        .is_err()
                    {
                        return;
                    }
                }
                std::thread::sleep(hold);
            }
        });
        format!("http://{addr}")
    }

    /// The marked model list split into `parts` byte ranges of near-equal size.
    /// No proper prefix of it parses as JSON, so only the last part completes it.
    fn marked_list_in_parts(parts: usize) -> Vec<Vec<u8>> {
        let bytes = LLAMA_SWAP_WITH_IMAGE.as_bytes();
        let size = bytes.len().div_ceil(parts);
        bytes.chunks(size).map(<[u8]>::to_vec).collect()
    }

    /// Head of a response with NO declared length: the body runs to the close.
    const UNDECLARED_LENGTH_HEAD: &str =
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n";

    /// Bytes that have already arrived are never thrown away. The body's last
    /// part lands 300 ms AFTER the deadline, inside one read that started
    /// before it, and the connection then stays open (so this is not the EOF
    /// path). The body is complete, so it is kept and the token advertised.
    /// A deadline checked BEFORE each read, or a check after it that ignores
    /// completeness, would discard it and cache `false` for HOST_PROBE_TTL.
    #[test]
    fn a_body_completed_just_past_the_deadline_is_kept() {
        let parts = marked_list_in_parts(2);
        let base = serve_scheduled(
            UNDECLARED_LENGTH_HEAD.to_string(),
            vec![
                (Duration::from_millis(600), parts[0].clone()),
                (Duration::from_millis(1300), parts[1].clone()),
            ],
            Duration::from_secs(5),
        );
        let ok = probe_endpoint(
            &base,
            Duration::from_millis(500),
            Duration::from_millis(1000),
        );
        assert!(
            ok,
            "a complete body that lands just past the deadline must advertise"
        );
    }

    /// The deadline starts once the client is BUILT. `Client::build` has no
    /// timeout of its own and is slow on a loaded box; here it takes 1.5 s of
    /// a 2 s deadline (the test seam), and the response then completes
    /// 1.5 s after the connection, inside the deadline counted from the build.
    /// A deadline taken before the build would expire mid-body (at +0.5 s, with
    /// two of three parts missing) and cache `false` for a healthy upstream.
    #[test]
    fn a_slow_client_build_does_not_spend_the_deadline() {
        let parts = marked_list_in_parts(3);
        let base = serve_scheduled(
            UNDECLARED_LENGTH_HEAD.to_string(),
            vec![
                (Duration::from_millis(200), parts[0].clone()),
                (Duration::from_millis(900), parts[1].clone()),
                (Duration::from_millis(1500), parts[2].clone()),
            ],
            Duration::ZERO,
        );
        TEST_BUILD_DELAY.with(|d| d.set(Duration::from_millis(1500)));
        let ok = probe_endpoint(&base, Duration::from_millis(500), Duration::from_secs(2));
        TEST_BUILD_DELAY.with(|d| d.set(Duration::ZERO));
        assert!(
            ok,
            "a slow client build must not spend the response's deadline"
        );
    }

    #[test]
    fn completeness_is_a_met_length_or_a_whole_json_document() {
        let full = LLAMA_SWAP_WITH_IMAGE.as_bytes();
        assert!(body_is_complete(full, None));
        assert!(body_is_complete(full, Some(full.len() as u64)));
        let half = &full[..full.len() / 2];
        assert!(
            !body_is_complete(half, None),
            "a prefix of the list never parses"
        );
        assert!(!body_is_complete(half, Some(full.len() as u64)));
        assert!(
            body_is_complete(half, Some(half.len() as u64)),
            "a met declared length"
        );
    }

    /// A server trickling the body one byte at a time, each byte well inside
    /// the per-read timeout, is cut off by the probe's deadline. Without the
    /// deadline loop this body would take ~20 s to arrive.
    #[test]
    fn a_trickling_server_is_cut_off_at_the_deadline() {
        let len = 200;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
             Content-Length: {len}\r\n\r\n"
        );
        let chunks = vec![b" ".to_vec(); len];
        let base = serve_raw(head, chunks, Duration::from_millis(100), Duration::ZERO);
        let timeout = Duration::from_millis(600);
        let started = Instant::now();
        let ok = probe_endpoint(&base, Duration::from_millis(300), timeout);
        assert!(
            !ok,
            "a body not complete by the deadline must not advertise"
        );
        assert!(
            started.elapsed() < timeout * 2 + Duration::from_millis(500),
            "the probe must end within 2 x its timeout, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_marker_must_match_exactly() {
        assert_eq!(
            image_model_ids(LLAMA_SWAP_WITH_IMAGE.as_bytes()),
            Some(vec!["qontinui-imagegen".to_string()])
        );
        let other_value =
            LLAMA_SWAP_WITH_IMAGE.replace("\"image_generation\"", "\"image_generation_v2\"");
        assert_eq!(image_model_ids(other_value.as_bytes()), Some(Vec::new()));
        let other_key = LLAMA_SWAP_WITH_IMAGE.replace("\"qontinui_capability\"", "\"capability\"");
        assert_eq!(image_model_ids(other_key.as_bytes()), Some(Vec::new()));
    }

    /// Each verdict holds for ITS OWN TTL: a cached answer is served while it is
    /// younger than that TTL, even when the probe would now answer otherwise,
    /// and is re-probed once it is not.
    #[test]
    fn the_cache_respects_the_ttl() {
        let cache: Mutex<Option<(bool, Instant)>> = Mutex::new(None);
        let (pos, neg) = (Duration::from_secs(25), Duration::from_secs(300));
        let calls = AtomicUsize::new(0);
        let answer = |v: bool| {
            calls.fetch_add(1, Ordering::SeqCst);
            v
        };
        let t0 = Instant::now();
        assert!(cached_probe(&cache, t0, pos, neg, || answer(true)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let inside = t0 + pos - Duration::from_millis(1);
        assert!(cached_probe(&cache, inside, pos, neg, || answer(false)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a fresh true must not re-probe"
        );
        // At the positive TTL: re-probed; the new verdict (a failure) is cached.
        let t1 = t0 + pos;
        assert!(!cached_probe(&cache, t1, pos, neg, || answer(false)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "an expired true must re-probe"
        );
        let inside = t1 + neg - Duration::from_millis(1);
        assert!(!cached_probe(&cache, inside, pos, neg, || answer(true)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a fresh false must not re-probe"
        );
        assert!(cached_probe(&cache, t1 + neg, pos, neg, || answer(true)));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "an expired false must re-probe"
        );
    }

    /// Over-advertising is the costly direction, so a `true` expires much
    /// sooner than a `false`: within one default heartbeat interval (30 s), and
    /// well inside `HOST_PROBE_TTL`. A cached `true` is therefore re-probed at
    /// an instant where a cached `false` is still served.
    #[test]
    fn a_positive_verdict_expires_sooner_than_a_negative_one() {
        assert!(POSITIVE_VERDICT_TTL < Duration::from_secs(30));
        assert!(POSITIVE_VERDICT_TTL < super::super::HOST_PROBE_TTL);
        let at = POSITIVE_VERDICT_TTL;
        let neg = super::super::HOST_PROBE_TTL;
        for (cached, expect_reprobe) in [(true, true), (false, false)] {
            let cache = Mutex::new(Some((cached, Instant::now())));
            let calls = AtomicUsize::new(0);
            let later = Instant::now() + at;
            let _ = cached_probe(&cache, later, POSITIVE_VERDICT_TTL, neg, || {
                calls.fetch_add(1, Ordering::SeqCst);
                !cached
            });
            assert_eq!(
                calls.load(Ordering::SeqCst) == 1,
                expect_reprobe,
                "a cached {cached} at POSITIVE_VERDICT_TTL: re-probe expected = {expect_reprobe}"
            );
        }
        // And the production path wires the asymmetric pair, not one TTL.
        let src = include_str!("imagegen_probe.rs");
        let (prod, _) = src
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("test module");
        let squeezed: String = prod.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            squeezed.contains("POSITIVE_VERDICT_TTL,super::HOST_PROBE_TTL,"),
            "capability_tokens must pass POSITIVE_VERDICT_TTL then HOST_PROBE_TTL"
        );
    }

    #[test]
    fn endpoint_resolution() {
        assert_eq!(resolve_endpoint(None), "http://127.0.0.1:8100");
        assert_eq!(resolve_endpoint(Some("   ")), "http://127.0.0.1:8100");
        assert_eq!(
            resolve_endpoint(Some(" http://10.0.0.5:8100/ ")),
            "http://10.0.0.5:8100"
        );
        assert_eq!(
            DEFAULT_IMAGEGEN_ENDPOINT,
            crate::mcp::ui_bridge::vision_ai::DEFAULT_ENDPOINT
        );
    }

    #[test]
    fn token_spelling_is_pinned() {
        assert_eq!(IMAGEGEN_CAPABILITY, "svc:imagegen");
        assert_eq!(tokens_for(true), vec!["svc:imagegen"]);
        assert!(tokens_for(false).is_empty());
    }
}
