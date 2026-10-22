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
//! * The request is bounded by [`CONNECT_TIMEOUT`] and [`REQUEST_TIMEOUT`], so
//!   a black-holed host costs the heartbeat at most ~2 s, and that on the
//!   blocking pool (`heartbeat_to_coord` runs `host_capabilities` through
//!   `spawn_blocking_tracked`), never on an async worker.
//! * The verdict — a `false` included — is cached for the parent's
//!   `HOST_PROBE_TTL` (300 s), like the PATH and Docker probes, so the 30 s
//!   heartbeat issues at most one request per TTL, and a withdrawn image
//!   service leaves `coord.devices.capabilities` within one TTL.
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

/// Whole-request budget, connect included. `/v1/models` is served from
/// llama-swap's config without touching any model, so it never waits on a
/// swap or a load.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// A model list larger than this is not a llama-swap answer; refuse it rather
/// than buffer it.
const MAX_BODY_BYTES: u64 = 1024 * 1024;

/// The cached verdict and when it was taken.
static IMAGEGEN_PROBE_CACHE: Mutex<Option<(bool, Instant)>> = Mutex::new(None);

/// The heartbeat's `svc:imagegen` contribution: the token, or nothing.
/// Cached for `HOST_PROBE_TTL`. BLOCKS on a cache miss (one bounded HTTP
/// request), so call it only off the async workers.
pub(crate) fn capability_tokens() -> Vec<String> {
    #[cfg(test)]
    if let Some(forced) = FORCED_VERDICT.with(std::cell::Cell::get) {
        return tokens_for(forced);
    }
    let available = cached_probe(
        &IMAGEGEN_PROBE_CACHE,
        Instant::now(),
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

/// Returns the cached verdict while it is younger than `ttl` at `now`;
/// otherwise runs `probe` and caches its verdict (a `false` included) as of
/// `now`. A poisoned lock degrades to probing uncached, never to a panic.
fn cached_probe(
    cache: &Mutex<Option<(bool, Instant)>>,
    now: Instant,
    ttl: Duration,
    probe: impl FnOnce() -> bool,
) -> bool {
    if let Ok(g) = cache.lock() {
        if let Some((verdict, taken)) = g.as_ref() {
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

/// `GET <base>/v1/models` and decide. Every failure is `false`.
fn probe_endpoint(base: &str, connect_timeout: Duration, timeout: Duration) -> bool {
    let url = format!("{base}/v1/models");
    let client = match reqwest::blocking::Client::builder()
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
    let resp = match client.get(&url).send() {
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
    let mut body = Vec::new();
    if let Err(e) = resp.take(MAX_BODY_BYTES + 1).read_to_end(&mut body) {
        debug!(
            "host_capabilities: {IMAGEGEN_CAPABILITY}: reading {url} failed ({e}) — not advertised"
        );
        return false;
    }
    if body.len() as u64 > MAX_BODY_BYTES {
        debug!("host_capabilities: {IMAGEGEN_CAPABILITY}: {url} body exceeds {MAX_BODY_BYTES} bytes — not advertised");
        return false;
    }
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

    /// An alias NAME is not a declaration: the image aliases listed without the
    /// marker (`includeAliasesInList: true`, or an unrelated config) advertise
    /// nothing — there is no hard-coded alias list.
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

    #[test]
    fn oversized_body_advertises_nothing() {
        let pad = " ".repeat(MAX_BODY_BYTES as usize);
        let body = format!("{LLAMA_SWAP_WITH_IMAGE}{pad}");
        assert!(!probe(&serve_once("200 OK", body)));
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

    #[test]
    fn the_cache_respects_the_ttl() {
        let cache: Mutex<Option<(bool, Instant)>> = Mutex::new(None);
        let ttl = Duration::from_secs(300);
        let calls = AtomicUsize::new(0);
        let t0 = Instant::now();
        let probe_true = || {
            calls.fetch_add(1, Ordering::SeqCst);
            true
        };
        assert!(cached_probe(&cache, t0, ttl, probe_true));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // Inside the TTL: served from the cache, even though the probe would
        // now answer differently.
        let probe_false = || {
            calls.fetch_add(1, Ordering::SeqCst);
            false
        };
        assert!(cached_probe(
            &cache,
            t0 + ttl - Duration::from_millis(1),
            ttl,
            probe_false
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a fresh verdict must not re-probe"
        );
        // At the TTL: re-probed, and the new verdict (a failure) is cached too.
        assert!(!cached_probe(&cache, t0 + ttl, ttl, probe_false));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "an expired verdict must re-probe"
        );
        assert!(!cached_probe(
            &cache,
            t0 + ttl + Duration::from_secs(1),
            ttl,
            probe_true
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a cached failure holds for its TTL"
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
