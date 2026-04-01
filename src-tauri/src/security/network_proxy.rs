//! Network mediation proxy.
//!
//! A lightweight TCP proxy that filters outbound network requests from
//! containerized step execution based on the [`NetworkPolicy`].
//!
//! # Architecture
//!
//! The proxy runs as a TCP server on `127.0.0.1` with an ephemeral port.
//! Containers are configured to use it via `HTTP_PROXY`/`HTTPS_PROXY` env vars.
//!
//! For HTTPS CONNECT: reads the first line to extract the target domain,
//! evaluates the policy, then either tunnels the connection or rejects it.
//!
//! The proxy does NOT perform TLS MITM — it filters at the domain level only.

use super::audit::{AuditDecision, AuditEventType, AuditLogger, SecurityAuditEvent};
use super::credential_proxy::CredentialProxy;
use super::engine::PolicyEngine;
use super::policy::SecurityPolicy;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

/// Network mediation proxy for filtering outbound requests from containers.
pub struct NetworkMediator {
    /// The address the proxy is listening on.
    listener_addr: SocketAddr,
    /// Shutdown signal sender.
    shutdown_tx: Option<oneshot::Sender<()>>,
}

/// Shared state for the proxy handler.
struct ProxyState {
    policy: SecurityPolicy,
    audit_logger: AuditLogger,
    #[allow(dead_code)]
    credential_proxy: Option<CredentialProxy>,
}

impl NetworkMediator {
    /// Start the network mediation proxy.
    ///
    /// Binds to `127.0.0.1:0` (ephemeral port) and starts serving.
    pub async fn start(
        policy: SecurityPolicy,
        audit_logger: AuditLogger,
        credential_proxy: Option<CredentialProxy>,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Failed to bind network proxy: {}", e))?;

        let addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get proxy address: {}", e))?;

        info!("Network mediation proxy started on {}", addr);

        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        let state = std::sync::Arc::new(ProxyState {
            policy,
            audit_logger,
            credential_proxy,
        });

        // Spawn the proxy accept loop
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept_result = listener.accept() => {
                        match accept_result {
                            Ok((stream, _peer)) => {
                                let state = state.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, state).await {
                                        debug!("Proxy connection error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                warn!("Proxy accept error: {}", e);
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("Network mediation proxy shutting down");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            listener_addr: addr,
            shutdown_tx: Some(shutdown_tx),
        })
    }

    /// Stop the proxy.
    pub fn stop(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }

    /// Get the proxy URL for use in container `HTTP_PROXY` / `HTTPS_PROXY` env vars.
    ///
    /// Uses `host.docker.internal` so containers on Docker Desktop (Windows/macOS)
    /// can reach the host-side proxy.
    pub fn proxy_url_for_container(&self) -> String {
        format!("http://host.docker.internal:{}", self.listener_addr.port())
    }

    /// Get the proxy address for host-side use.
    pub fn proxy_addr(&self) -> SocketAddr {
        self.listener_addr
    }
}

impl Drop for NetworkMediator {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

/// Handle a single proxy connection.
///
/// Reads the HTTP request headers to determine if this is a CONNECT tunnel
/// or a regular HTTP request, extracts the target domain, and applies policy.
async fn handle_connection(
    mut stream: TcpStream,
    state: std::sync::Arc<ProxyState>,
) -> Result<(), String> {
    // Read request headers into a buffer without consuming the stream.
    // We use peek-style reading: read bytes into a Vec, parse the first line,
    // then drain remaining headers before passing the stream to the tunnel.
    let mut buf_reader = BufReader::new(&mut stream);

    // Read the first line (e.g., "CONNECT api.anthropic.com:443 HTTP/1.1\r\n")
    let mut first_line = String::new();
    buf_reader
        .read_line(&mut first_line)
        .await
        .map_err(|e| format!("Failed to read request line: {}", e))?;

    let first_line_trimmed = first_line.trim().to_string();
    if first_line_trimmed.is_empty() {
        return Ok(());
    }

    let parts: Vec<&str> = first_line_trimmed.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(());
    }

    let method = parts[0].to_string();
    let target = parts[1].to_string();

    if method.eq_ignore_ascii_case("CONNECT") {
        // HTTPS CONNECT tunnel
        let (domain, port) = parse_authority(&target);
        let protocol = "https";

        // Drain remaining headers (we don't need them for CONNECT)
        loop {
            let mut header_line = String::new();
            buf_reader
                .read_line(&mut header_line)
                .await
                .map_err(|e| format!("Failed to read header: {}", e))?;
            if header_line.trim().is_empty() {
                break;
            }
        }

        // Drop the BufReader to release the borrow on stream
        drop(buf_reader);

        handle_connect_tunnel(stream, &domain, port, protocol, &state).await
    } else {
        // Plain HTTP request — read remaining headers for credential injection
        let mut headers = Vec::new();
        loop {
            let mut header_line = String::new();
            buf_reader
                .read_line(&mut header_line)
                .await
                .map_err(|e| format!("Failed to read header: {}", e))?;
            if header_line.trim().is_empty() {
                break;
            }
            headers.push(header_line);
        }

        // Extract Host from headers or URL
        let domain = headers
            .iter()
            .find(|h| h.to_lowercase().starts_with("host:"))
            .and_then(|h| {
                // Split "Host: value" on first colon to separate header name from value
                h.split_once(':').map(|(_, v)| {
                    let v = v.trim();
                    // Handle IPv6 literal addresses like [::1]:8080
                    if v.starts_with('[') {
                        v.split(']')
                            .next()
                            .unwrap_or("")
                            .trim_start_matches('[')
                            .to_string()
                    } else {
                        // Strip port from host:port
                        v.split(':').next().unwrap_or("").to_string()
                    }
                })
            })
            .unwrap_or_else(|| extract_domain_from_url(&target));
        let protocol = "http";

        // Evaluate policy
        let evaluation = PolicyEngine::evaluate_network(&state.policy, &domain, protocol);

        // Drop the BufReader to release the borrow on stream
        drop(buf_reader);

        match evaluation {
            Err(denial) => {
                log_network_event(&state, &domain, protocol, AuditDecision::Denied, Some(&denial.reason));
                warn!("Network proxy blocked HTTP: {} - {}", domain, denial.reason);
                send_http_error(stream, 403, &denial.reason).await
            }
            Ok(()) => {
                log_network_event(&state, &domain, protocol, AuditDecision::Allowed, None);
                handle_http_forward(
                    stream,
                    &method,
                    &target,
                    &first_line,
                    &headers,
                    &state,
                )
                .await
            }
        }
    }
}

/// Handle a CONNECT tunnel request.
async fn handle_connect_tunnel(
    mut stream: TcpStream,
    domain: &str,
    port: u16,
    protocol: &str,
    state: &std::sync::Arc<ProxyState>,
) -> Result<(), String> {
    // Evaluate policy
    let evaluation = PolicyEngine::evaluate_network(&state.policy, domain, protocol);

    match evaluation {
        Err(denial) => {
            log_network_event(state, domain, protocol, AuditDecision::Denied, Some(&denial.reason));
            warn!("Network proxy blocked CONNECT: {} - {}", domain, denial.reason);

            let response = format!(
                "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\n\r\n{}",
                denial.reason.len(),
                denial.reason
            );
            stream
                .write_all(response.as_bytes())
                .await
                .map_err(|e| format!("Failed to send 403: {}", e))?;
            Ok(())
        }
        Ok(()) => {
            log_network_event(state, domain, protocol, AuditDecision::Allowed, None);

            // Connect to upstream
            let upstream_addr = format!("{}:{}", domain, port);
            debug!("CONNECT tunnel to {}", upstream_addr);

            match TcpStream::connect(&upstream_addr).await {
                Ok(upstream) => {
                    // Send 200 Connection Established
                    stream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .map_err(|e| format!("Failed to send 200: {}", e))?;

                    // Bidirectional copy
                    let (mut client_read, mut client_write) = stream.into_split();
                    let (mut upstream_read, mut upstream_write) = upstream.into_split();

                    let c2u = tokio::io::copy(&mut client_read, &mut upstream_write);
                    let u2c = tokio::io::copy(&mut upstream_read, &mut client_write);

                    tokio::select! {
                        r = c2u => { if let Err(e) = r { debug!("Tunnel c→u: {}", e); } }
                        r = u2c => { if let Err(e) = r { debug!("Tunnel u→c: {}", e); } }
                    }

                    Ok(())
                }
                Err(e) => {
                    let msg = format!("Failed to connect to {}: {}", upstream_addr, e);
                    error!("{}", msg);
                    let response = format!(
                        "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\n\r\n{}",
                        msg.len(),
                        msg
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    Ok(())
                }
            }
        }
    }
}

/// Send an HTTP error response.
async fn send_http_error(mut stream: TcpStream, status: u16, message: &str) -> Result<(), String> {
    let status_text = match status {
        403 => "Forbidden",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        status_text,
        message.len(),
        message
    );
    stream
        .write_all(response.as_bytes())
        .await
        .map_err(|e| format!("Failed to send error response: {}", e))?;
    Ok(())
}

/// Forward an HTTP request to the upstream server.
///
/// Reconstructs the request from parsed headers, performs credential injection
/// if a CredentialProxy is configured, forwards via a direct TCP connection,
/// and relays the response back to the client.
async fn handle_http_forward(
    mut stream: TcpStream,
    method: &str,
    target: &str,
    original_request_line: &str,
    headers: &[String],
    state: &std::sync::Arc<ProxyState>,
) -> Result<(), String> {
    // Parse upstream address from target URL
    let domain = extract_domain_from_url(target);
    let port = if let Some((_host, port_str)) = target
        .strip_prefix("http://")
        .unwrap_or(target)
        .split_once(':')
    {
        port_str
            .split('/')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(80)
    } else {
        80
    };

    let upstream_addr = format!("{}:{}", domain, port);

    // Reconstruct the request with credential injection
    let mut request_bytes = Vec::new();
    request_bytes.extend_from_slice(original_request_line.as_bytes());

    for header in headers {
        let mut header_line = header.clone();

        // Perform credential injection on relevant headers
        if let Some(ref cred_proxy) = state.credential_proxy {
            if let Some((name, _rest)) = header.split_once(':') {
                if cred_proxy.should_scan_header(name.trim()) {
                    let value = header
                        .split_once(':')
                        .map(|(_, v)| v.trim())
                        .unwrap_or("");
                    if let Some((replaced_value, cred_name)) =
                        cred_proxy.resolve_placeholder(value)
                    {
                        header_line =
                            format!("{}: {}\r\n", name.trim(), replaced_value);
                        // Log credential injection (without the actual value)
                        state.audit_logger.log(
                            SecurityAuditEvent::new(
                                AuditEventType::CredentialInjection,
                                format!("Injected credential '{}' for {}", cred_name, domain),
                                AuditDecision::Allowed,
                            )
                            .with_metadata(serde_json::json!({
                                "domain": domain,
                                "header": name.trim(),
                                "credential": cred_name,
                            })),
                        );
                    }
                }
            }
        }

        request_bytes.extend_from_slice(header_line.as_bytes());
    }
    // End of headers
    request_bytes.extend_from_slice(b"\r\n");

    // Connect to upstream and forward
    match TcpStream::connect(&upstream_addr).await {
        Ok(mut upstream) => {
            // Send the request
            upstream
                .write_all(&request_bytes)
                .await
                .map_err(|e| format!("Failed to write to upstream: {}", e))?;

            // Bidirectional copy for response (and any request body)
            let (mut client_read, mut client_write) = stream.into_split();
            let (mut upstream_read, mut upstream_write) = upstream.into_split();

            let c2u = tokio::io::copy(&mut client_read, &mut upstream_write);
            let u2c = tokio::io::copy(&mut upstream_read, &mut client_write);

            tokio::select! {
                r = c2u => { if let Err(e) = r { debug!("HTTP fwd c→u: {}", e); } }
                r = u2c => { if let Err(e) = r { debug!("HTTP fwd u→c: {}", e); } }
            }

            Ok(())
        }
        Err(e) => {
            let msg = format!("Failed to connect to {}: {}", upstream_addr, e);
            error!("{}", msg);
            send_http_error(stream, 502, &msg).await
        }
    }
}

/// Log a network audit event.
fn log_network_event(
    state: &ProxyState,
    domain: &str,
    protocol: &str,
    decision: AuditDecision,
    reason: Option<&str>,
) {
    if state.policy.network.log_requests {
        let mut event = SecurityAuditEvent::new(
            AuditEventType::NetworkRequest,
            format!("{}://{}", protocol, domain),
            decision,
        );
        if let Some(r) = reason {
            event = event.with_reason(r);
        }
        state.audit_logger.log(event);
    }
}

/// Parse "host:port" authority from a CONNECT target.
fn parse_authority(authority: &str) -> (String, u16) {
    if let Some((host, port_str)) = authority.rsplit_once(':') {
        let port = port_str.parse::<u16>().unwrap_or(443);
        (host.to_string(), port)
    } else {
        (authority.to_string(), 443)
    }
}

/// Extract domain from a URL (for plain HTTP requests).
fn extract_domain_from_url(url: &str) -> String {
    // URL like "http://example.com/path" or just "example.com/path"
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or(url);

    without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_authority() {
        let (host, port) = parse_authority("api.anthropic.com:443");
        assert_eq!(host, "api.anthropic.com");
        assert_eq!(port, 443);

        let (host, port) = parse_authority("example.com:8080");
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);

        let (host, port) = parse_authority("example.com");
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_extract_domain_from_url() {
        assert_eq!(
            extract_domain_from_url("http://api.example.com/v1/chat"),
            "api.example.com"
        );
        assert_eq!(
            extract_domain_from_url("https://api.example.com:8443/path"),
            "api.example.com"
        );
        assert_eq!(
            extract_domain_from_url("api.example.com/path"),
            "api.example.com"
        );
    }
}
