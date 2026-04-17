//! Cloud relay transport for physical device UI Bridge connections.
//!
//! Creates a local TCP listener that tunnels HTTP requests through a WebSocket
//! connection to the qontinui.io backend, which relays them to the mobile device.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::TransportError;

// ============================================================================
// Tunnel protocol types
// ============================================================================

/// A tunneled HTTP request sent from the runner to the backend relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelRequest {
    pub id: String,
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

/// A tunneled HTTP response received from the backend relay (originated from device).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelResponse {
    pub id: String,
    pub status: u16,
    pub headers: HashMap<String, String>,
    pub body: Option<serde_json::Value>,
}

// ============================================================================
// TunnelHandle
// ============================================================================

/// Handle to a running tunnel. Dropping or calling shutdown stops the tunnel.
pub struct TunnelHandle {
    pub local_port: u16,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    task_handle: tokio::task::JoinHandle<()>,
}

impl TunnelHandle {
    /// Signal the tunnel to shut down and wait for the task to finish.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.task_handle.await;
    }
}

// ============================================================================
// CloudTransport
// ============================================================================

/// Manages WebSocket-based relay tunnels through the qontinui.io backend.
pub struct CloudTransport {
    backend_url: String,
    tunnels: Arc<Mutex<HashMap<String, TunnelHandle>>>,
}

impl CloudTransport {
    pub fn new(backend_url: String) -> Self {
        Self {
            backend_url,
            tunnels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Open a tunnel for `device_id`.
    ///
    /// Connects to `wss://{backend}/api/v1/device-bridge/tunnel/{device_id}?token={auth_token}`
    /// via tokio-tungstenite, starts a local TCP listener on `127.0.0.1:0`, and returns the
    /// local port. HTTP requests arriving on that port are forwarded over the WebSocket and
    /// their responses are written back.
    pub async fn open_tunnel(
        &self,
        device_id: &str,
        auth_token: &str,
    ) -> Result<u16, TransportError> {
        let ws_url = build_ws_url(&self.backend_url, device_id, auth_token);

        info!("[cloud-transport] Connecting to relay: {}", ws_url);

        let (ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| TransportError::PairingError(format!("WS connect failed: {e}")))?;

        // Bind local listener on any available port
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| TransportError::ProxyBindFailed(e.to_string()))?;

        let local_port = listener
            .local_addr()
            .map_err(|e| TransportError::ProxyBindFailed(e.to_string()))?
            .port();

        info!(
            "[cloud-transport] Tunnel for {} listening on 127.0.0.1:{}",
            device_id, local_port
        );

        // Channel for routing WS responses to the waiting HTTP handlers
        // Key: request ID -> oneshot sender
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        // Split the WebSocket stream
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        // Channel for sending TunnelRequests from local TCP handlers to the WS sender
        let (req_tx, mut req_rx) = mpsc::channel::<TunnelRequest>(64);

        let pending_for_rx = pending.clone();

        // Task: drive the WebSocket — receive responses and forward requests
        let task_handle = tokio::spawn(async move {
            let mut ws_tx_opt = Some(ws_tx);
            // Heartbeat every 30s to keep the Redis TTL fresh on the backend
            let mut heartbeat_interval = tokio::time::interval(std::time::Duration::from_secs(30));
            heartbeat_interval.tick().await; // fire first tick immediately (ignored)
            loop {
                tokio::select! {
                    // Periodic heartbeat to keep the relay alive
                    _ = heartbeat_interval.tick() => {
                        if let Some(ref mut tx) = ws_tx_opt {
                            let hb = serde_json::json!({ "type": "heartbeat" });
                            if let Err(e) = tx.send(Message::Text(hb.to_string().into())).await {
                                warn!("[cloud-transport] Heartbeat send failed: {e}");
                                break;
                            }
                        }
                    }

                    // Forward outbound tunnel requests to the WebSocket
                    req = req_rx.recv() => {
                        match req {
                            None => break,
                            Some(tunnel_req) => {
                                if let Some(ref mut tx) = ws_tx_opt {
                                    // Wrap as { "type": "tunnel_request", ...fields } to match
                                    // the backend device_bridge_ws.py message discriminator.
                                    let wrapped = match serde_json::to_value(&tunnel_req) {
                                        Ok(serde_json::Value::Object(mut map)) => {
                                            map.insert(
                                                "type".to_string(),
                                                serde_json::Value::String("tunnel_request".to_string()),
                                            );
                                            serde_json::Value::Object(map)
                                        }
                                        Ok(other) => other,
                                        Err(e) => {
                                            error!("[cloud-transport] Serialization error: {e}");
                                            continue;
                                        }
                                    };
                                    match serde_json::to_string(&wrapped) {
                                        Ok(json) => {
                                            if let Err(e) = tx.send(Message::Text(json.into())).await {
                                                error!("[cloud-transport] WS send error: {e}");
                                                break;
                                            }
                                        }
                                        Err(e) => {
                                            error!("[cloud-transport] Serialization error: {e}");
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Receive inbound tunnel responses from the WebSocket
                    msg = ws_rx.next() => {
                        match msg {
                            None | Some(Err(_)) => {
                                warn!("[cloud-transport] WebSocket connection closed");
                                break;
                            }
                            Some(Ok(Message::Text(text))) => {
                                // Parse as generic JSON first, then dispatch on `type` field.
                                let value: serde_json::Value = match serde_json::from_str(&text) {
                                    Ok(v) => v,
                                    Err(e) => {
                                        warn!("[cloud-transport] Failed to parse WS message: {e}");
                                        continue;
                                    }
                                };
                                let msg_type = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                match msg_type {
                                    "tunnel_response" => {
                                        match serde_json::from_value::<TunnelResponse>(value) {
                                            Ok(resp) => {
                                                let mut pending_map = pending_for_rx.lock().await;
                                                if let Some(tx) = pending_map.remove(&resp.id) {
                                                    let _ = tx.send(resp);
                                                } else {
                                                    warn!(
                                                        "[cloud-transport] No pending request for response id {}",
                                                        resp.id
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                warn!("[cloud-transport] Failed to parse tunnel response: {e}");
                                            }
                                        }
                                    }
                                    "tunnel_connected" => {
                                        info!("[cloud-transport] Tunnel handshake confirmed by backend");
                                    }
                                    "heartbeat_ack" => {
                                        debug!("[cloud-transport] Heartbeat ack received");
                                    }
                                    "error" => {
                                        let msg = value.get("message")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown error");
                                        error!("[cloud-transport] Backend error: {msg}");
                                    }
                                    other => {
                                        debug!("[cloud-transport] Unknown message type: {other}");
                                    }
                                }
                            }
                            Some(Ok(Message::Close(_))) => {
                                info!("[cloud-transport] WebSocket close frame received");
                                break;
                            }
                            Some(Ok(_)) => {} // ping/pong/binary — ignore
                        }
                    }

                    // Shutdown signal
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("[cloud-transport] Shutdown signal received");
                            break;
                        }
                    }
                }
            }
        });

        let pending_for_accept = pending.clone();
        let req_tx_for_accept = req_tx.clone();
        let mut shutdown_rx2 = shutdown_tx.subscribe();

        // Task: accept local TCP connections and proxy them over the tunnel
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer)) => {
                                debug!("[cloud-transport] Accepted local connection from {}", peer);
                                let pending = pending_for_accept.clone();
                                let req_tx = req_tx_for_accept.clone();
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        handle_local_connection(stream, pending, req_tx).await
                                    {
                                        debug!("[cloud-transport] Local connection error: {e}");
                                    }
                                });
                            }
                            Err(e) => {
                                warn!("[cloud-transport] Accept error: {e}");
                            }
                        }
                    }
                    _ = shutdown_rx2.changed() => {
                        if *shutdown_rx2.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        let handle = TunnelHandle {
            local_port,
            shutdown_tx,
            task_handle,
        };

        self.tunnels
            .lock()
            .await
            .insert(device_id.to_string(), handle);

        Ok(local_port)
    }

    /// Close the tunnel for a specific device.
    pub async fn close_tunnel(&self, device_id: &str) {
        let handle = self.tunnels.lock().await.remove(device_id);
        if let Some(h) = handle {
            info!("[cloud-transport] Closing tunnel for {}", device_id);
            h.shutdown().await;
        }
    }

    /// Close all active tunnels.
    pub async fn close_all(&self) {
        let handles: Vec<(String, TunnelHandle)> = self.tunnels.lock().await.drain().collect();
        for (device_id, handle) in handles {
            info!("[cloud-transport] Closing tunnel for {}", device_id);
            handle.shutdown().await;
        }
    }
}

// ============================================================================
// Local TCP connection handler
// ============================================================================

/// Handle one incoming local HTTP/1.1 connection: parse the request, forward via
/// the tunnel, wait for the response, and write it back.
async fn handle_local_connection(
    stream: TcpStream,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<TunnelResponse>>>>,
    req_tx: mpsc::Sender<TunnelRequest>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // ── Read request line ────────────────────────────────────────────────────
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let request_line = request_line.trim_end();
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    // HTTP version is ignored

    // ── Read headers ────────────────────────────────────────────────────────
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(": ") {
            let key = k.to_ascii_lowercase();
            if key == "content-length" {
                content_length = v.parse().unwrap_or(0);
            }
            headers.insert(key, v.to_string());
        }
    }

    // ── Read body ───────────────────────────────────────────────────────────
    let body: Option<serde_json::Value> = if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        reader.read_exact(&mut buf).await?;
        serde_json::from_slice(&buf).ok()
    } else {
        None
    };

    // ── Build and send tunnel request ────────────────────────────────────────
    let id = Uuid::new_v4().to_string();
    let (resp_tx, resp_rx) = oneshot::channel::<TunnelResponse>();

    {
        let mut pending_map = pending.lock().await;
        pending_map.insert(id.clone(), resp_tx);
    }

    let tunnel_req = TunnelRequest {
        id: id.clone(),
        method,
        path,
        headers,
        body,
    };

    req_tx.send(tunnel_req).await.map_err(|e| {
        Box::new(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            format!("Tunnel channel closed: {e}"),
        )) as Box<dyn std::error::Error + Send + Sync>
    })?;

    // ── Wait for response (5 s timeout) ─────────────────────────────────────
    let tunnel_resp = match tokio::time::timeout(std::time::Duration::from_secs(5), resp_rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => {
            // sender dropped
            pending.lock().await.remove(&id);
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "Tunnel response sender dropped",
            )));
        }
        Err(_) => {
            // timeout
            pending.lock().await.remove(&id);
            let resp = "HTTP/1.1 504 Gateway Timeout\r\nContent-Length: 0\r\n\r\n";
            write_half.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    };

    // ── Write HTTP response ──────────────────────────────────────────────────
    let body_bytes = match &tunnel_resp.body {
        Some(v) => serde_json::to_vec(v).unwrap_or_default(),
        None => vec![],
    };

    let status_text = status_text(tunnel_resp.status);
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n",
        tunnel_resp.status,
        status_text,
        body_bytes.len()
    );

    for (k, v) in &tunnel_resp.headers {
        response.push_str(&format!("{}: {}\r\n", k, v));
    }
    response.push_str("\r\n");

    write_half.write_all(response.as_bytes()).await?;
    if !body_bytes.is_empty() {
        write_half.write_all(&body_bytes).await?;
    }

    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// Convert the backend URL to a WebSocket URL and append the tunnel path.
fn build_ws_url(backend_url: &str, device_id: &str, auth_token: &str) -> String {
    let ws_base = backend_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let ws_base = ws_base.trim_end_matches('/');
    format!(
        "{}/api/v1/device-bridge/ws/device-bridge/tunnel/{}?token={}",
        ws_base,
        urlencoding::encode(device_id),
        urlencoding::encode(auth_token),
    )
}

/// Map a status code to its standard reason phrase.
fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}
