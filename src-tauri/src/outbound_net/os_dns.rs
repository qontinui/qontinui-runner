//! The runner's DNS resolver for every async `reqwest` client: the OPERATING
//! SYSTEM's resolver (`getaddrinfo`), run on a small bounded set of dedicated
//! threads rather than on tokio's blocking pool.
//!
//! # Why not hickory
//!
//! The runner briefly built every `reqwest::Client` with the `hickory-dns`
//! feature. Hickory does not ask the OS to resolve a name; it reads the list
//! of nameservers configured on every network adapter
//! (`hickory_resolver::system_conf::read_system_conf`) and queries those
//! servers itself, with a 5 s per-query timeout. That list is not what the OS
//! actually uses. Windows routes a query by interface metric, NRPT rules and a
//! VPN's DNS leak protection; hickory sees none of that.
//!
//! Measured on 2026-10-18 on a box running a WireGuard VPN (ProtonVPN), whose
//! leak protection drops DNS to the physical adapter's ISP servers: hickory's
//! system config listed the VPN's server and both blocked ISP servers. Every
//! FRESH hickory resolver took 5.5 s to resolve `coord.qontinui.io` (one
//! blocked server's 5 s timeout, then an answer), against 0.37 s through
//! `getaddrinfo`. reqwest builds a fresh resolver per `Client`, so every
//! per-request client, including the 30 s device-register heartbeat with its
//! 5 s cap, timed out before it sent a byte. The box's
//! `coord_drain_state` went UNKNOWN for 33 hours and autonomous spawns paused,
//! although coord answered the same request from the same box in 0.4 s.
//! The runner was never slow; the resolver was.
//!
//! # Why not plain `GaiResolver` on the shared client either
//!
//! reqwest's default `GaiResolver` also calls `getaddrinfo`, but it does so
//! through `tokio::task::spawn_blocking`, which takes a slot on the RUNTIME's
//! blocking pool. On the main runtime that pool is shared with every other
//! blocking task, and it has been pinned near its 512-thread ceiling before,
//! which queued DNS behind unrelated work until requests timed out. That is
//! the incident the bin's shared `coord_http::coord_client` was introduced for.
//! [`OsResolver`] keeps the OS's answer and avoids that queue by running each
//! lookup on its own short-lived thread, gated by a process-wide semaphore of
//! [`MAX_CONCURRENT_LOOKUPS`] permits. A stuck lookup can therefore hold one
//! permit, never a blocking-pool slot.
//!
//! # Every async client goes through here, and clippy enforces it
//!
//! Build clients with [`client_builder`] (in place of
//! `reqwest::Client::builder()` / `reqwest::ClientBuilder::new()`) or
//! [`client`] (in place of `reqwest::Client::new()`), both re-exported as
//! `qontinui_runner_lib::outbound_net::{client_builder, client}`:
//!
//! ```ignore
//! let client = qontinui_runner_lib::outbound_net::client_builder()
//!     .timeout(std::time::Duration::from_secs(5))
//!     .build()?;
//! ```
//!
//! Two guards keep it that way. The raw constructors are `disallowed-methods`
//! in the repo-root `clippy.toml`, at `deny` (`src-tauri/Cargo.toml`
//! `[lints.clippy]`), so a new raw builder in lib or bin code fails clippy.
//! This module holds the one sanctioned call, under an `#[expect]` that
//! `expect_ratchet.rs` counts as a door rather than as grandfathered debt.
//! Clippy alone is not enough: the PR clippy leg does not compile test code,
//! and an existing fn-level `#[expect(clippy::disallowed_methods)]` for
//! `Row::get` would also silence a reqwest call added to that fn. So
//! `tests::no_raw_async_reqwest_constructor_outside_the_door` scans
//! `src-tauri/src` and `src-tauri/tests` as text, under `cargo test`.
//! Per-client settings (timeouts, proxies, TLS roots, pool sizes) chain on the
//! returned builder exactly as they did on the raw one: the resolver is the
//! only thing these constructors set.
//!
//! `reqwest::blocking::Client` is NOT covered and need not be: a blocking
//! client drives its requests on its own dedicated thread and runtime, so its
//! `GaiResolver` lookups take that private runtime's blocking pool, never the
//! main runtime's. A `reqwest::Client` reached through a generic `Default`
//! (`#[derive(Default)]` on a struct holding one, `Result::unwrap_or_default`
//! on a build) also bypasses the lint, which matches only named paths; the
//! source scan refuses the `unwrap_or_default` spelling.

use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::sync::Semaphore;

/// The most `getaddrinfo` calls this process runs at once through
/// [`OsResolver`]. A lookup over the limit waits asynchronously for a permit;
/// it does not take a thread while it waits.
///
/// 32, not the 8 this started at when only four shared clients used it: every
/// async `reqwest::Client` in the runner now resolves here (module doc), and
/// reqwest keeps no DNS cache, so each new connection is a lookup. The bound
/// exists to cap THREADS held by hung lookups; it must not become the
/// throughput limit. A resolver that hangs for 5 s per query (the VPN shape in
/// the module doc) would let 8 permits clear only 1.6 lookups/s, and every
/// request queued behind them would hit [`LOOKUP_TIMEOUT`]. 32 short-lived
/// threads in the worst case are cheap; tokio's blocking pool, which this
/// replaces for these lookups, allows 512.
pub const MAX_CONCURRENT_LOOKUPS: usize = 32;

/// The longest [`OsResolver`] waits for one name, permit wait included. A
/// Windows `getaddrinfo` against an unresponsive server can block well past
/// this; the lookup thread then finishes on its own and its answer is
/// discarded, but the REQUEST fails here with an error naming the host rather
/// than hanging on a caller that set no per-request timeout.
pub const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A [`reqwest::dns::Resolve`] that asks the operating system's resolver.
///
/// Cheap to clone; every instance shares the one process-wide permit pool.
#[derive(Clone, Debug, Default)]
pub struct OsResolver;

fn permits() -> &'static Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_LOOKUPS)))
}

/// The shared [`OsResolver`], as the `Arc` that
/// [`reqwest::ClientBuilder::dns_resolver`] takes.
pub fn os_resolver() -> Arc<OsResolver> {
    static RESOLVER: OnceLock<Arc<OsResolver>> = OnceLock::new();
    RESOLVER.get_or_init(|| Arc::new(OsResolver)).clone()
}

/// A `reqwest::ClientBuilder` that resolves through [`OsResolver`] — the
/// runner's only way to start an async client (module doc). reqwest's default
/// `GaiResolver` would take a slot on the calling runtime's blocking pool per
/// lookup.
///
/// Keep the body to this one call: the fn-level `#[expect]` would also hide
/// any other disallowed method added here.
#[expect(
    clippy::disallowed_methods,
    reason = "the one sanctioned reqwest::Client::builder() call: it installs OsResolver"
)]
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().dns_resolver(os_resolver())
}

/// `client_builder().build()`, for the sites that used `reqwest::Client::new()`.
///
/// # Panics
///
/// Exactly where `reqwest::Client::new()` panics: when the TLS backend cannot
/// be initialized or the system configuration cannot be loaded.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("reqwest client with the OS resolver (the same build reqwest::Client::new() does)")
}

/// Resolve `host` through the OS resolver on a dedicated thread, holding one
/// of the [`MAX_CONCURRENT_LOOKUPS`] permits for the duration.
///
/// The returned addresses carry port 0; reqwest substitutes the URL's port.
pub async fn resolve_host(host: String) -> Result<Vec<SocketAddr>, BoxError> {
    resolve_host_with(permits().clone(), host).await
}

/// [`resolve_host`] against an explicit permit pool, so a test can observe
/// the pool without racing every other lookup in the process.
async fn resolve_host_with(
    permits: Arc<Semaphore>,
    host: String,
) -> Result<Vec<SocketAddr>, BoxError> {
    let permit = permits
        .acquire_owned()
        .await
        .map_err(|e| -> BoxError { format!("dns permit pool closed: {e}").into() })?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let thread_host = host.clone();
    std::thread::Builder::new()
        .name("runner-dns".to_string())
        .spawn(move || {
            // `(host, 0).to_socket_addrs()` is `getaddrinfo`. The permit is
            // released only when the OS call returns, so a hung lookup is
            // counted against the bound for as long as it actually runs.
            let result = (thread_host.as_str(), 0u16)
                .to_socket_addrs()
                .map(|it| it.collect::<Vec<_>>());
            drop(permit);
            let _ = tx.send(result);
        })
        .map_err(|e| -> BoxError { format!("spawn dns thread for {host}: {e}").into() })?;
    let addrs = rx
        .await
        .map_err(|_| -> BoxError {
            format!("dns thread for {host} ended without an answer").into()
        })?
        .map_err(|e| -> BoxError { format!("resolve {host}: {e}").into() })?;
    if addrs.is_empty() {
        return Err(format!("resolve {host}: the OS resolver returned no addresses").into());
    }
    Ok(addrs)
}

impl Resolve for OsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = tokio::time::timeout(LOOKUP_TIMEOUT, resolve_host(host.clone()))
                .await
                .map_err(|_| -> BoxError {
                    format!(
                        "resolve {host}: no answer from the OS resolver within {LOOKUP_TIMEOUT:?}"
                    )
                    .into()
                })??;
            let it: Addrs = Box::new(addrs.into_iter());
            Ok(it)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn resolves_localhost_through_the_os() {
        let addrs = resolve_host("localhost".to_string())
            .await
            .expect("the OS resolver answers for localhost");
        assert!(addrs.iter().all(|a| a.ip().is_loopback()), "{addrs:?}");
        assert!(addrs.iter().all(|a| a.port() == 0), "{addrs:?}");
    }

    #[tokio::test]
    async fn an_unresolvable_name_is_an_error_not_an_empty_answer() {
        // `.invalid` is reserved (RFC 6761) and never resolves.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            resolve_host("no-such-host.invalid".to_string()),
        )
        .await
        .expect("the OS resolver answers within 30 s");
        match result {
            // The normal case: the error names the host it failed on.
            Err(err) => assert!(err.to_string().contains("no-such-host.invalid"), "{err}"),
            // An NXDOMAIN-rewriting resolver (some ISPs) answers with a
            // search page's address. That is the network's answer, not ours —
            // but it must never be a loopback address or an empty list.
            Ok(addrs) => assert!(
                !addrs.is_empty() && addrs.iter().all(|a| !a.ip().is_loopback()),
                "{addrs:?}"
            ),
        }
    }

    #[tokio::test]
    async fn every_permit_is_returned_after_lookups_complete() {
        let pool = Arc::new(Semaphore::new(2));
        let lookups: Vec<_> = (0..6)
            .map(|_| tokio::spawn(resolve_host_with(pool.clone(), "localhost".to_string())))
            .collect();
        for l in lookups {
            l.await.expect("join").expect("localhost resolves");
        }
        // Each lookup thread drops its permit BEFORE it sends the answer, so
        // every permit is back once every answer has been received.
        assert_eq!(pool.available_permits(), 2);
    }

    /// The lookup must not depend on the calling runtime's blocking pool: a
    /// current-thread runtime whose blocking pool is capped at ONE thread, and
    /// that one thread parked, still resolves.
    #[test]
    fn resolves_while_the_runtime_blocking_pool_is_exhausted() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        rt.block_on(async {
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let parked = tokio::task::spawn_blocking(move || {
                let _ = release_rx.recv();
            });
            let addrs = tokio::time::timeout(
                Duration::from_secs(10),
                resolve_host("localhost".to_string()),
            )
            .await
            .expect("resolution must not queue behind the parked blocking task")
            .expect("localhost resolves");
            assert!(!addrs.is_empty());
            release_tx.send(()).expect("release the parked task");
            parked.await.expect("join the parked task");
        });
    }

    /// The root-cause pin: no `reqwest` in the lockfile may depend on
    /// `hickory-resolver`. A cargo feature is crate-global, so `hickory-dns`
    /// switched on from ANY manifest — this crate's `reqwest` line, a
    /// `[dependencies.reqwest]` table, a `[features]` entry, or another
    /// dependency via feature unification — makes every `reqwest::Client` in
    /// the process skip the OS resolver (module doc). The lockfile records the
    /// unified result, so it is the one place every route shows up.
    #[test]
    fn no_reqwest_in_the_lockfile_depends_on_hickory() {
        let lock = include_str!("../../../Cargo.lock");
        // Line-based, so a CRLF checkout of the lockfile matches too.
        let is_reqwest = |block: &str| block.lines().any(|l| l.trim_end() == "name = \"reqwest\"");
        let reqwest_blocks: Vec<&str> = lock
            .split("[[package]]")
            .filter(|b| is_reqwest(b))
            .collect();
        // Guard the guard: finding no reqwest at all would make this vacuous.
        assert!(
            !reqwest_blocks.is_empty(),
            "Cargo.lock no longer lists reqwest where this test looks"
        );
        let offenders: Vec<&str> = reqwest_blocks
            .iter()
            .filter(|b| b.contains("\"hickory-resolver"))
            .map(|b| {
                b.lines()
                    .find(|l| l.starts_with("version = "))
                    .unwrap_or("version = ?")
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "reqwest must resolve through the OS, not hickory (see outbound_net::os_dns); \
             reqwest {offenders:?} depends on hickory-resolver in Cargo.lock"
        );
    }

    /// What every migrated call site relies on: a client from
    /// [`client_builder`] completes a request to a NAME (so a lookup happens)
    /// while the runtime's blocking pool is exhausted. reqwest's default
    /// `GaiResolver` would queue the lookup behind the parked task and miss the
    /// deadline, so a `client_builder` that stopped installing [`OsResolver`]
    /// reds this test.
    #[test]
    fn a_client_builder_client_requests_while_the_blocking_pool_is_exhausted() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .expect("runtime");
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let port = listener.local_addr().expect("addr").port();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                while let Ok((mut sock, _)) = listener.accept().await {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock
                        .write_all(b"HTTP/1.1 204 No Content\r\nX-Os-Dns-Test: this-listener\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                }
            });
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let parked = tokio::task::spawn_blocking(move || {
                let _ = release_rx.recv();
            });
            let client = client_builder()
                .no_proxy()
                .build()
                .expect("client");
            let resp = tokio::time::timeout(
                Duration::from_secs(10),
                client.get(format!("http://localhost:{port}/")).send(),
            )
            .await
            .expect("the lookup must not queue behind the parked blocking task")
            .expect("request through client_builder()");
            assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
            // `localhost` may resolve to ::1 first; the marker proves the
            // answer came from THIS IPv4 listener, not another process's.
            assert_eq!(
                resp.headers()
                    .get("x-os-dns-test")
                    .and_then(|v| v.to_str().ok()),
                Some("this-listener")
            );
            release_tx.send(()).expect("release the parked task");
            parked.await.expect("join the parked task");
        });
    }

    /// Raw async constructors, as a CODE line names them (string literals and
    /// comments are masked out first, see [`mask_code`]).
    const RAW_QUALIFIED: [&str; 5] = [
        "reqwest::Client::new",
        "reqwest::Client::builder",
        "reqwest::Client::default",
        "reqwest::ClientBuilder::new",
        "reqwest::ClientBuilder::default",
    ];
    /// The constructor names a bare `<imported name>::` may be followed by.
    const RAW_BARE_FNS: [&str; 3] = ["new(", "builder(", "default("];
    /// The line of the one sanctioned raw call, in [`client_builder`].
    const DOOR_LINE: &str = "reqwest::Client::builder().dns_resolver(os_resolver())";

    /// The file's lines with string-literal CONTENTS blanked and `//`
    /// comments dropped, so a constructor quoted in a string or a comment is
    /// not code. String state carries across lines (a `\`-continued message).
    /// Raw strings (`r#"…"#`) are not modelled; none quote a constructor.
    fn mask_code(text: &str) -> Vec<String> {
        let mut in_string = false;
        let mut out = Vec::new();
        for line in text.lines() {
            let chars: Vec<char> = line.chars().collect();
            let mut masked = String::with_capacity(line.len());
            let mut k = 0;
            while k < chars.len() {
                let c = chars[k];
                if in_string {
                    if c == '\\' {
                        masked.push_str("  ");
                        k += 2;
                        continue;
                    }
                    if c == '"' {
                        in_string = false;
                        masked.push('"');
                    } else {
                        masked.push(' ');
                    }
                    k += 1;
                    continue;
                }
                if c == '/' && chars.get(k + 1) == Some(&'/') {
                    break;
                }
                if c == '"' {
                    in_string = true;
                    masked.push('"');
                    k += 1;
                    continue;
                }
                // A char literal: '"' or '\"' must not open a string.
                if c == '\'' {
                    if chars.get(k + 1) == Some(&'\\') && chars.get(k + 3) == Some(&'\'') {
                        masked.push_str("' '");
                        k += 4;
                        continue;
                    }
                    if chars.get(k + 2) == Some(&'\'') {
                        masked.push_str("' '");
                        k += 3;
                        continue;
                    }
                }
                masked.push(c);
                k += 1;
            }
            out.push(masked);
        }
        out
    }

    /// The names a bare constructor call may use in this file: `Client` /
    /// `ClientBuilder` (and any `as` alias) from a `use reqwest::…;`
    /// statement, grouped or not, that is not the blocking client's.
    fn imported_client_names(masked: &[String]) -> Vec<String> {
        let joined = masked.join("\n");
        let mut names = Vec::new();
        let mut rest = joined.as_str();
        while let Some(at) = rest.find("use reqwest::") {
            let tail = rest.get(at..).unwrap_or("");
            let end = tail.find(';').unwrap_or(tail.len());
            let stmt = tail.get(..end).unwrap_or("");
            rest = tail.get(end..).unwrap_or("");
            if stmt.contains("blocking") {
                continue;
            }
            let tokens: Vec<&str> = stmt
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .filter(|t| !t.is_empty())
                .collect();
            for (j, t) in tokens.iter().enumerate() {
                if *t == "Client" || *t == "ClientBuilder" {
                    if tokens.get(j + 1) == Some(&"as") {
                        if let Some(alias) = tokens.get(j + 2) {
                            names.push((*alias).to_string());
                        }
                    } else {
                        names.push((*t).to_string());
                    }
                }
            }
        }
        names
    }

    /// Every raw-constructor hit in one file's code, as `line: text`.
    fn raw_constructor_hits(text: &str) -> Vec<String> {
        let raw: Vec<&str> = text.lines().collect();
        let masked = mask_code(text);
        let names = imported_client_names(&masked);
        let mut hits = Vec::new();
        for (i, line) in masked.iter().enumerate() {
            let qualified = RAW_QUALIFIED.iter().any(|p| line.contains(p));
            // A bare `Client::new(` counts only where nothing path-like (an
            // identifier char or `:`) precedes it, so `EmbeddingClient::new(`
            // and `reqwest::blocking::Client::new(` do not.
            let bare = names.iter().any(|name| {
                RAW_BARE_FNS.iter().any(|f| {
                    let pat = format!("{name}::{f}");
                    line.match_indices(&pat).any(|(at, _)| {
                        line.get(..at)
                            .and_then(|before| before.chars().next_back())
                            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == ':'))
                    })
                })
            });
            // `client_builder()…build().unwrap_or_default()` reaches
            // `Client::default()` through a generic, which clippy cannot see.
            // Judged over the whole statement: back to the previous line that
            // ends one (`;`, `{`, `}`), comments masked, whitespace removed.
            let default_fallback = line.contains(".unwrap_or_default()") && {
                let mut start = i;
                while start > 0 && i - start < 64 {
                    let prev = masked[start - 1].trim_end();
                    if prev.ends_with(';') || prev.ends_with('{') || prev.ends_with('}') {
                        break;
                    }
                    start -= 1;
                }
                let stmt: String = masked[start..=i]
                    .concat()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                stmt.contains("client_builder()")
                    && (stmt.contains(".build().unwrap_or_default()")
                        || stmt.contains(".build().ok().unwrap_or_default()"))
            };
            if qualified || bare || default_fallback {
                hits.push(format!(
                    "{}: {}",
                    i + 1,
                    raw.get(i).map_or("", |l| l.trim())
                ));
            }
        }
        hits
    }

    /// The source-level half of the guard (module doc): no raw async reqwest
    /// constructor in the code of `src-tauri/src` or `src-tauri/tests`, test
    /// code included. This file is scanned too, and may hold exactly one hit:
    /// the door's own call.
    #[test]
    fn no_raw_async_reqwest_constructor_outside_the_door() {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let door = manifest.join("src").join("outbound_net").join("os_dns.rs");
        let mut stack = vec![manifest.join("src"), manifest.join("tests")];
        let (mut walked, mut offenders, mut door_hits) = (0usize, Vec::new(), Vec::new());
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read_dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    // Generated DB code, not ours.
                    if path.file_name().and_then(|f| f.to_str()) != Some("clorinde") {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                walked += 1;
                let text = std::fs::read_to_string(&path).expect("read source");
                let hits = raw_constructor_hits(&text);
                if path == door {
                    door_hits = hits;
                    continue;
                }
                for hit in hits {
                    offenders.push(format!("{}:{hit}", path.display()));
                }
            }
        }
        // Guard the guard: a walker that lost the tree reports a clean zero.
        assert!(walked > 1000, "walked only {walked} .rs files");
        // The door's file holds exactly its one call — which also proves the
        // matcher still sees a raw call at all.
        assert!(
            door_hits.len() == 1 && door_hits[0].contains(DOOR_LINE),
            "{} must hold exactly one raw constructor, the door's `{DOOR_LINE}`; found {door_hits:#?}",
            door.display()
        );
        assert!(
            offenders.is_empty(),
            "build async reqwest clients with qontinui_runner_lib::outbound_net::client_builder() \
             or ::client() (OS resolver, off tokio's blocking pool), not a raw constructor: \
             {offenders:#?}"
        );
    }

    /// The matcher on the shapes it must and must not flag. Each shape is a
    /// string here, which the masking itself keeps out of this file's hits.
    #[test]
    fn the_raw_constructor_matcher_flags_exactly_the_raw_shapes() {
        let flagged = |src: &str| !raw_constructor_hits(src).is_empty();
        // Qualified calls, and a function-pointer spelling.
        assert!(flagged("let c = reqwest::Client::new();"));
        assert!(flagged("let c = reqwest::Client::builder().build()?;"));
        assert!(flagged("let c = reqwest::ClientBuilder::new();"));
        assert!(flagged("CELL.get_or_init(reqwest::Client::new);"));
        assert!(flagged(
            "let c: reqwest::Client = reqwest::Client::default();"
        ));
        // Bare calls under every import shape.
        assert!(flagged(
            "use reqwest::Client;\nfn f() { let c = Client::new(); }"
        ));
        assert!(flagged(
            "use reqwest::Client;\nfn f() { let c = Client::builder(); }"
        ));
        assert!(flagged(
            "use reqwest::{Client, StatusCode};\nlet c = Client::new();"
        ));
        assert!(flagged(
            "use reqwest::{\n    StatusCode,\n    Client,\n};\nlet c = Client::new();"
        ));
        assert!(flagged(
            "use reqwest::Client as Http;\nlet c = Http::builder();"
        ));
        assert!(flagged(
            "use reqwest::ClientBuilder;\nlet b = ClientBuilder::new();"
        ));
        // The generic-Default fallback, on one line, split, through `.ok()`,
        // and with comments between the chain calls (the coord_sync shape).
        assert!(flagged(
            "let c = qontinui_runner_lib::outbound_net::client_builder().build().unwrap_or_default();"
        ));
        assert!(flagged(
            "let c = qontinui_runner_lib::outbound_net::client_builder()\n    .build()\n    .unwrap_or_default();"
        ));
        assert!(flagged(
            "let c = client_builder().build().ok().unwrap_or_default();"
        ));
        assert!(flagged(
            "let c = client_builder()\n    // one\n    .timeout(t)\n    // two\n    // three\n    // four\n    // five\n    // six\n    // seven\n    // eight\n    .pool_max_idle_per_host(32)\n    .build()\n    // why the fallback\n    .unwrap_or_default();"
        ));
        // Not raw: the door's helpers, the blocking client, other `Client`s,
        // comments, strings, and an unrelated `unwrap_or_default`.
        assert!(!flagged(
            "let c = qontinui_runner_lib::outbound_net::client_builder().build()?;"
        ));
        assert!(!flagged("let c = reqwest::blocking::Client::new();"));
        assert!(!flagged(
            "let c = reqwest::blocking::Client::builder().build()?;"
        ));
        assert!(!flagged(
            "use reqwest::blocking::Client;\nlet c = Client::new();"
        ));
        assert!(!flagged(
            "use reqwest::Client;\nlet e = EmbeddingClient::new();"
        ));
        assert!(!flagged("let m = mcp::Client::new();"));
        assert!(!flagged("let c = Client::new();"));
        assert!(!flagged("// reqwest::Client::new() in a comment"));
        assert!(!flagged("let x = 1; // reqwest::Client::new() trailing"));
        assert!(!flagged("let s = \"reqwest::Client::new()\";"));
        assert!(!flagged(
            "let q = '\"'; let s = \"reqwest::Client::new()\";"
        ));
        assert!(!flagged("let v = map.get(k).cloned().unwrap_or_default();"));
        assert!(!flagged(
            "let c = qontinui_runner_lib::outbound_net::client();\nmatch c.get(u).send().await {\n    Ok(resp) => resp.text().await.unwrap_or_default(),"
        ));
        assert!(!flagged(
            "let c = qontinui_runner_lib::outbound_net::client_builder().build()?;\nlet t = resp.text().await.unwrap_or_default();"
        ));
    }

    /// `reqwest` accepts the resolver and drives a real request through it.
    #[tokio::test]
    async fn a_reqwest_client_resolves_through_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .await;
            }
        });
        let client = client_builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client");
        // `localhost` may resolve to ::1 first; the listener is IPv4, and
        // hyper falls back across the returned addresses.
        let resp = client
            .get(format!("http://localhost:{port}/"))
            .send()
            .await
            .expect("request through the OS resolver");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }
}
