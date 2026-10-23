//! Rate-limit a transport-error WARN to once per `(site, host, kind)` per
//! window, carrying the count of identical lines it suppressed.
//!
//! # Why
//!
//! Plan `2026-09-24-runner-coord-credential-stranded-after-outage` Phase 1
//! step 2. When the resolver failed for `coord.qontinui.io` on 2026-09-24, the
//! runner wrote 201,120 identical `dns error: no records found for
//! coord.qontinui.io.home.arpa.` lines that day (102,565 the next morning) —
//! ~205k of them from ONE site, `fleet::tree_publisher`, which logs per repo
//! per cycle. The volume buried every other signal in the log, including the
//! credential expiring underneath it. One line per window with a suppressed
//! count says the same thing and leaves the log readable.
//!
//! This is logging only. It changes no retry, no backoff and no resolver —
//! the resolver itself is owned by plan
//! `2026-09-16-runner-long-lived-http-clients-freeze-dns-state-across-network-changes`.
//!
//! # Keying
//!
//! `(site, host, kind)`: a DIFFERENT failure — another host, or the same host
//! failing a different way (DNS → connect refused → timeout) — is new
//! information and logs immediately. Only the identical repeat is suppressed.
//! The map is bounded ([`TRANSPORT_LOG_MAX_KEYS`]).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an identical `(site, host, kind)` failure stays quiet after it
/// last logged.
pub(crate) const TRANSPORT_LOG_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Most distinct keys remembered. Past this the least-recently-emitted key is
/// evicted, so a flood of distinct hosts can never grow the map without bound
/// (an evicted key simply logs again the next time it fails).
pub(crate) const TRANSPORT_LOG_MAX_KEYS: usize = 256;

/// Coarse class of a transport failure, derived from the error chain. Coarse
/// on purpose: it is a dedupe key, and the full chain still goes in the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TransportErrorKind {
    Dns,
    Timeout,
    Connect,
    Other,
}

impl TransportErrorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TransportErrorKind::Dns => "dns",
            TransportErrorKind::Timeout => "timeout",
            TransportErrorKind::Connect => "connect",
            TransportErrorKind::Other => "other",
        }
    }

    /// Classify from the rendered error chain plus reqwest's own predicates.
    /// DNS is checked first: reqwest reports a resolver failure as a CONNECT
    /// error, and "connect" would hide the one distinction that matters here.
    pub(crate) fn classify(chain: &str, is_timeout: bool, is_connect: bool) -> Self {
        let lower = chain.to_ascii_lowercase();
        if lower.contains("dns error")
            || lower.contains("failed to lookup address")
            || lower.contains("no records found")
        {
            TransportErrorKind::Dns
        } else if is_timeout || lower.contains("timed out") {
            TransportErrorKind::Timeout
        } else if is_connect {
            TransportErrorKind::Connect
        } else {
            TransportErrorKind::Other
        }
    }
}

/// What the caller should do with this occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogDecision {
    /// Log it. `suppressed` identical occurrences were swallowed since the
    /// last emitted line for this key (0 on the first).
    Emit { suppressed: u64 },
    /// Swallow it; it is counted toward the next emitted line.
    Suppress,
}

#[derive(Debug)]
struct Entry {
    last_emitted: Instant,
    suppressed: u64,
}

/// The dedupe state. One process-global instance serves every site
/// ([`warn_transport_error`]); tests build their own with a short window.
#[derive(Debug)]
pub(crate) struct TransportLogDedupe {
    window: Duration,
    entries: Mutex<HashMap<(String, String, TransportErrorKind), Entry>>,
}

impl TransportLogDedupe {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Decide for one occurrence at `now`.
    pub(crate) fn decide(
        &self,
        site: &str,
        host: &str,
        kind: TransportErrorKind,
        now: Instant,
    ) -> LogDecision {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let key = (site.to_string(), host.to_string(), kind);
        match entries.get_mut(&key) {
            Some(e) if now.saturating_duration_since(e.last_emitted) < self.window => {
                e.suppressed = e.suppressed.saturating_add(1);
                LogDecision::Suppress
            }
            Some(e) => {
                let suppressed = e.suppressed;
                e.last_emitted = now;
                e.suppressed = 0;
                LogDecision::Emit { suppressed }
            }
            None => {
                if entries.len() >= TRANSPORT_LOG_MAX_KEYS {
                    if let Some(oldest) = entries
                        .iter()
                        .min_by_key(|(_, e)| e.last_emitted)
                        .map(|(k, _)| k.clone())
                    {
                        entries.remove(&oldest);
                    }
                }
                entries.insert(
                    key,
                    Entry {
                        last_emitted: now,
                        suppressed: 0,
                    },
                );
                LogDecision::Emit { suppressed: 0 }
            }
        }
    }
}

fn global() -> &'static TransportLogDedupe {
    static GLOBAL: std::sync::OnceLock<TransportLogDedupe> = std::sync::OnceLock::new();
    GLOBAL.get_or_init(|| TransportLogDedupe::new(TRANSPORT_LOG_WINDOW))
}

/// Host of `url`, or `"unparseable"` when it has none. Never the full URL:
/// a path or query in the key would split one failing host into many keys
/// (and put request detail into a long-lived map).
pub(crate) fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "unparseable".to_string())
}

/// `warn!` a reqwest send failure at most once per `(site, host, kind)` per
/// [`TRANSPORT_LOG_WINDOW`]. `context` is the site's own prefix (e.g.
/// `"fleet::tree_publisher: POST <url> for <repo>"`); the full error chain is
/// appended, and the line says how many identical failures it stands for.
pub(crate) fn warn_transport_error(site: &str, url: &str, context: &str, err: &reqwest::Error) {
    let chain = crate::util::error_chain::error_chain(err);
    let kind = TransportErrorKind::classify(&chain, err.is_timeout(), err.is_connect());
    let host = host_of(url);
    match global().decide(site, &host, kind, Instant::now()) {
        LogDecision::Suppress => {}
        LogDecision::Emit { suppressed: 0 } => {
            tracing::warn!(
                "{context} failed: {chain} [{} error for {host}; identical repeats are \
                 logged at most once per {}s]",
                kind.as_str(),
                TRANSPORT_LOG_WINDOW.as_secs()
            );
        }
        LogDecision::Emit { suppressed } => {
            tracing::warn!(
                "{context} failed: {chain} [{} error for {host}; {suppressed} identical \
                 failure(s) suppressed in the last {}s]",
                kind.as_str(),
                TRANSPORT_LOG_WINDOW.as_secs()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_incident_line_as_dns_not_connect() {
        let chain = "error sending request for url (https://coord.qontinui.io/coord/trees/upsert): \
                     client error (Connect): dns error: proto error: no records found for Query \
                     { name: Name(\"coord.qontinui.io.home.arpa.\"), query_type: A, query_class: IN }";
        assert_eq!(
            TransportErrorKind::classify(chain, false, true),
            TransportErrorKind::Dns
        );
        assert_eq!(
            TransportErrorKind::classify("connection refused", false, true),
            TransportErrorKind::Connect
        );
        assert_eq!(
            TransportErrorKind::classify("operation timed out", false, false),
            TransportErrorKind::Timeout
        );
    }

    /// The incident's shape: thousands of identical failures in one window
    /// become ONE line, and the next line after the window carries the count.
    #[test]
    fn identical_failures_log_once_per_window_with_a_suppressed_count() {
        let d = TransportLogDedupe::new(Duration::from_secs(300));
        let t0 = Instant::now();
        let site = "fleet::tree_publisher";
        let host = "coord.qontinui.io";
        assert_eq!(
            d.decide(site, host, TransportErrorKind::Dns, t0),
            LogDecision::Emit { suppressed: 0 }
        );
        for i in 1..=1000u64 {
            assert_eq!(
                d.decide(
                    site,
                    host,
                    TransportErrorKind::Dns,
                    t0 + Duration::from_millis(i)
                ),
                LogDecision::Suppress
            );
        }
        assert_eq!(
            d.decide(
                site,
                host,
                TransportErrorKind::Dns,
                t0 + Duration::from_secs(301)
            ),
            LogDecision::Emit { suppressed: 1000 }
        );
        // The counter restarted with that line.
        assert_eq!(
            d.decide(
                site,
                host,
                TransportErrorKind::Dns,
                t0 + Duration::from_secs(302)
            ),
            LogDecision::Suppress
        );
    }

    /// A DIFFERENT failure is new information: another kind, host or site
    /// logs immediately even inside the window.
    #[test]
    fn a_different_host_kind_or_site_is_not_suppressed() {
        let d = TransportLogDedupe::new(Duration::from_secs(300));
        let t0 = Instant::now();
        assert!(matches!(
            d.decide("s", "coord.qontinui.io", TransportErrorKind::Dns, t0),
            LogDecision::Emit { .. }
        ));
        assert!(matches!(
            d.decide("s", "coord.qontinui.io", TransportErrorKind::Connect, t0),
            LogDecision::Emit { .. }
        ));
        assert!(matches!(
            d.decide("s", "api.qontinui.io", TransportErrorKind::Dns, t0),
            LogDecision::Emit { .. }
        ));
        assert!(matches!(
            d.decide("other", "coord.qontinui.io", TransportErrorKind::Dns, t0),
            LogDecision::Emit { .. }
        ));
    }

    #[test]
    fn the_key_map_is_bounded() {
        let d = TransportLogDedupe::new(Duration::from_secs(300));
        let t0 = Instant::now();
        for i in 0..(TRANSPORT_LOG_MAX_KEYS + 50) {
            let _ = d.decide(
                "s",
                &format!("h{i}"),
                TransportErrorKind::Dns,
                t0 + Duration::from_millis(i as u64),
            );
        }
        assert_eq!(d.entries.lock().unwrap().len(), TRANSPORT_LOG_MAX_KEYS);
        // The oldest were evicted, the newest kept.
        assert!(!d.entries.lock().unwrap().contains_key(&(
            "s".into(),
            "h0".into(),
            TransportErrorKind::Dns
        )));
    }

    #[test]
    fn host_of_parses_urls_and_never_collapses_to_empty() {
        assert_eq!(
            host_of("https://coord.qontinui.io/coord/trees/upsert"),
            "coord.qontinui.io"
        );
        assert_eq!(host_of("not a url"), "unparseable");
        assert_eq!(host_of("/coord/trees/upsert?x=1"), "unparseable");
    }
}
