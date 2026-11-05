//! Name an outbound request failure by its MECHANISM.
//!
//! reqwest's `error sending request for url (…)` is the same line whether the
//! name did not resolve, the TCP connect was refused, the TLS handshake failed
//! or a timer fired late because its runtime was starved. On spaceship
//! (2026-10-19) those lines arrived in same-second bursts across nine modules
//! and three hosts — what a starved runtime looks like when it is finally
//! scheduled — and nothing in the log could say which (plan
//! `2026-10-19-runner-ui-wedge-signal-fires-when-the-app-runtime-is-starved-by-process-spawns`,
//! Phase 0 item 4). [`failure_kind`] gives each line a `kind=`, and
//! [`current_runtime_label`] its `rt=`.

/// `timeout` | `connect` | `dns` | `tls` | `body` | `other`.
///
/// Order matters:
/// * `is_decode` is answered FIRST: a decode error's chain is a serde message
///   that can name a field such as `tls_mode`, which the text walk would
///   misread as a TLS failure.
/// * a DNS failure surfaces as a connect error whose source chain names the
///   resolver, and a TLS failure as a connect error whose chain names the
///   handshake, so the chain is read BEFORE `is_connect` decides.
/// * the chain walk starts at `e.source()`, never at `e` itself: reqwest's own
///   `Display` appends the request URL, and a path such as `/certificates`
///   would read as TLS.
pub fn failure_kind(e: &reqwest::Error) -> &'static str {
    if e.is_decode() {
        return "body";
    }
    if e.is_timeout() {
        return "timeout";
    }
    if let Some(kind) = std::error::Error::source(e).and_then(chain_kind) {
        return kind;
    }
    if e.is_connect() {
        return "connect";
    }
    if e.is_body() {
        return "body";
    }
    "other"
}

/// The source-chain half of [`failure_kind`], public so it can be tested on a
/// constructed chain (a real `reqwest::Error` cannot be built by hand).
pub fn chain_kind(e: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(err) = cur {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::TimedOut {
                return Some("timeout");
            }
        }
        let text = err.to_string().to_ascii_lowercase();
        if text.contains("dns error")
            || text.contains("failed to lookup address")
            || text.contains("no such host")
            || text.contains("name or service not known")
        {
            return Some("dns");
        }
        if text.contains("tls")
            || text.contains("certificate")
            || text.contains("handshake")
            || text.contains("rustls")
        {
            return Some("tls");
        }
        cur = err.source();
    }
    None
}

/// The current thread's name (`app-rt`, `mcp-api-rt`, `fleet-hb-rt`, …), for
/// the `rt=` label. `unnamed` when the thread carries none.
pub fn current_runtime_label() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Wrap(
        &'static str,
        Option<Box<dyn std::error::Error + Send + Sync>>,
    );
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|e| e as _)
        }
    }

    fn chain(outer: &'static str, inner: Box<dyn std::error::Error + Send + Sync>) -> Wrap {
        Wrap(
            outer,
            Some(Box::new(Wrap("client error (Connect)", Some(inner)))),
        )
    }

    #[test]
    fn the_chain_names_dns_tls_and_io_timeouts() {
        let dns = chain(
            "error sending request",
            Box::new(Wrap("dns error: no such host is known", None)),
        );
        assert_eq!(chain_kind(&dns), Some("dns"));
        let tls = chain(
            "error sending request",
            Box::new(Wrap("invalid peer certificate: UnknownIssuer", None)),
        );
        assert_eq!(chain_kind(&tls), Some("tls"));
        let io = chain(
            "error sending request",
            Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "connect timed out",
            )),
        );
        assert_eq!(chain_kind(&io), Some("timeout"));
        let refused = chain(
            "error sending request",
            Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            )),
        );
        assert_eq!(
            chain_kind(&refused),
            None,
            "a plain refusal is left to `is_connect`"
        );
    }

    #[tokio::test]
    async fn a_refused_connect_reads_connect_and_a_slow_server_reads_timeout() {
        // Refused: bind and drop to get a closed loopback port.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = super::super::os_dns::client_builder().build().unwrap();
        let err = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .expect_err("nothing listens");
        assert_eq!(failure_kind(&err), "connect");

        // Timeout: a listener that accepts and never answers.
        let silent = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = silent.local_addr().unwrap();
        let _keep = std::thread::spawn(move || {
            let _held: Vec<_> = silent.incoming().take(1).collect();
            std::thread::sleep(std::time::Duration::from_secs(5));
        });
        let err = client
            .get(format!("http://{addr}/"))
            .timeout(std::time::Duration::from_millis(300))
            .send()
            .await
            .expect_err("never answers");
        assert_eq!(failure_kind(&err), "timeout");
    }

    /// The URL is in reqwest's own `Display`; a path naming `certificates`
    /// must not turn a plain refusal into `tls`.
    #[tokio::test]
    async fn a_refused_connect_to_a_certificates_path_reads_connect() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = super::super::os_dns::client_builder().build().unwrap();
        let err = client
            .get(format!("http://127.0.0.1:{port}/api/v1/certificates/tls"))
            .send()
            .await
            .expect_err("nothing listens");
        assert!(
            err.to_string().contains("certificates"),
            "the fixture relies on the URL being in the Display: {err}"
        );
        assert_eq!(failure_kind(&err), "connect");
    }

    /// A decode error whose serde message names a `tls_mode` field is a body
    /// failure, not a TLS one.
    #[tokio::test]
    async fn a_decode_error_naming_a_tls_field_reads_body() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let _server = std::thread::spawn(move || {
            if let Some(Ok(mut stream)) = listener.incoming().next() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = r#"{"other":1}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct Wants {
            tls_mode: String,
        }
        let client = super::super::os_dns::client_builder().build().unwrap();
        let err = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("the local server answers")
            .json::<Wants>()
            .await
            .expect_err("the body lacks `tls_mode`");
        assert!(err.is_decode(), "{err:?}");
        assert!(
            format!("{err:?}").contains("tls_mode"),
            "the fixture relies on the chain naming the field: {err:?}"
        );
        assert_eq!(failure_kind(&err), "body");
    }

    #[test]
    fn the_runtime_label_is_the_thread_name() {
        let label = std::thread::Builder::new()
            .name("mcp-api-rt".into())
            .spawn(current_runtime_label)
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(label, "mcp-api-rt");
    }
}
