//! Does a refusal ack survive the server dropping the socket right after
//! `send(Text); send(Close)`, when the client reads it only afterwards?
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::any;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as C;

#[derive(Clone, Copy, Debug)]
enum Mode { DropAtOnce, AwaitPeerClose }

async fn drive(socket: WebSocket, mode: Mode) {
    let (mut sink, mut stream) = socket.split();
    let _ = stream.next().await; // the register frame
    let _ = sink.send(Message::Text("{\"type\":\"ack\",\"ok\":false}".into())).await;
    let _ = sink.send(Message::Close(None)).await;
    if let Mode::AwaitPeerClose = mode {
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(Ok(m)) = stream.next().await {
                if matches!(m, Message::Close(_)) { break; }
            }
        }).await;
    }
}

async fn serve(mode: Mode) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let app = Router::new().route("/ws", any(move |ws: WebSocketUpgrade| async move {
        ws.on_upgrade(move |s| drive(s, mode))
    }));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    port
}

async fn one(port: u16, delay: Duration) -> Result<(), String> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws")).await.map_err(|e| format!("connect {e}"))?;
    ws.send(C::Text("{\"type\":\"register\"}".into())).await.map_err(|e| format!("send {e}"))?;
    tokio::time::sleep(delay).await;
    let first = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
    // Finish the close handshake the way a real client would.
    let res = match first {
        Ok(Some(Ok(C::Text(_)))) => Ok(()),
        other => Err(format!("{other:?}")),
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), async { while let Some(Ok(_)) = ws.next().await {} }).await;
    res
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    for mode in [Mode::DropAtOnce, Mode::AwaitPeerClose] {
        for delay_ms in [0u64, 200] {
            let port = serve(mode).await;
            let mut lost = 0; let mut total = 0; let mut kinds = std::collections::BTreeMap::new();
            for _round in 0..10 {
                let mut hs = Vec::new();
                for _ in 0..50 { hs.push(tokio::spawn(one(port, Duration::from_millis(delay_ms)))); }
                for h in hs {
                    total += 1;
                    if let Err(e) = h.await.unwrap() { lost += 1; *kinds.entry(e.chars().take(120).collect::<String>()).or_insert(0) += 1; }
                }
            }
            println!("RESULT mode={mode:?} client_read_delay={delay_ms}ms lost={lost}/{total} kinds={kinds:?}");
        }
    }
}
