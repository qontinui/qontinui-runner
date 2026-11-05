//! Test-only loopback coord that reports whether every response body it
//! served was read to the end (plan 2026-10-28-one-session-lifecycle-authority,
//! Phase 85).
//!
//! An unread body is the defect under test, so each body is far larger than
//! the loopback socket buffers can absorb: the server's body stream can only
//! reach its end when the client actually reads it. A caller that drops the
//! response unread closes the connection and the stream never completes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, Uri};
use axum::response::Response;

/// 64 KiB per chunk x 256 = 16 MiB per body.
const CHUNK_LEN: usize = 64 * 1024;
const CHUNKS: usize = 256;
static CHUNK: [u8; CHUNK_LEN] = [b' '; CHUNK_LEN];

/// A fixed answer for one exact path, served instead of the probe body (not
/// counted). For a caller whose control flow depends on what coord said.
#[derive(Clone)]
pub(crate) struct Fixed {
    pub path: String,
    pub status: u16,
    pub body: &'static str,
}

pub(crate) struct DrainProbe {
    pub base: String,
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}

impl DrainProbe {
    /// Answer every request with `status` and a 16 MiB streamed body.
    pub async fn spawn(status: u16) -> Self {
        Self::spawn_with(status, Vec::new()).await
    }

    pub async fn spawn_with(status: u16, fixed: Vec<Fixed>) -> Self {
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let (s, c) = (started.clone(), completed.clone());
        let fixed = Arc::new(fixed);
        let app = axum::Router::new().fallback(move |uri: Uri| {
            let (s, c, fixed) = (s.clone(), c.clone(), fixed.clone());
            async move {
                if let Some(f) = fixed.iter().find(|f| f.path == uri.path()) {
                    return Response::builder()
                        .status(f.status)
                        .body(Body::from(f.body))
                        .unwrap();
                }
                s.fetch_add(1, Ordering::SeqCst);
                let stream = futures::stream::unfold(0usize, move |i| {
                    let c = c.clone();
                    async move {
                        if i < CHUNKS {
                            Some((Ok::<_, std::io::Error>(Bytes::from_static(&CHUNK)), i + 1))
                        } else {
                            // Polled past the last chunk: the client read it all.
                            c.fetch_add(1, Ordering::SeqCst);
                            None
                        }
                    }
                });
                Response::builder()
                    .status(StatusCode::from_u16(status).unwrap())
                    .body(Body::from_stream(stream))
                    .unwrap()
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            base: format!("http://{addr}"),
            started,
            completed,
        }
    }

    /// Panics unless at least one probe body was served and every one was
    /// read to its end.
    pub async fn assert_all_drained(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let (s, c) = (
                self.started.load(Ordering::SeqCst),
                self.completed.load(Ordering::SeqCst),
            );
            if s > 0 && c == s {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("response body left unread: {c} of {s} probe bodies read to the end");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
