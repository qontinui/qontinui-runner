//! PTY output pipe — taps a session's terminal output and publishes it to
//! coord's warm/hot/cold retention tiers. Plan
//! [`2026-05-23-coord-native-sessions-phase-7-10`] §Phase 8 (plan §D10 +
//! §D11).
//!
//! ## When it runs
//!
//! Only when the session's [`Intent::share_output`] is `true`. Default
//! `true` as of plan
//! `2026-09-22-transcript-sync-default-on-with-tenant-and-user-controls`
//! §3.5 (ship-on with a reachable off-switch, per `engineering-priorities`
//! `capability-ships-enabled`) — a session that explicitly opts out (or an
//! Intent body that carried an explicit `false` before the flip) still pays
//! **zero overhead**: no task is spawned, no receiver is subscribed, no
//! bytes leave the machine.
//!
//! ## What it does
//!
//! 1. Subscribes to the transport's output broadcast (the same
//!    base64-encoded chunk stream the runner frontend renders — see
//!    [`crate::terminal::TerminalSession::subscribe_output`]).
//! 2. Decodes each chunk to raw bytes.
//! 3. When [`Intent::effective_redact_secrets`] is on, runs a fast regex
//!    sweep masking `key=value`-shaped secrets. **Defense in depth, NOT a
//!    security boundary** (plan §D11) — a determined leak (multi-line
//!    secrets, base64 blobs, custom formats) still gets through. The
//!    operator opts a session into sharing; redaction is a courtesy
//!    backstop, documented as such.
//! 4. Coalesces + rate-limits: buffers bytes and flushes on whichever
//!    comes first — a ~[`FLUSH_INTERVAL`] timer tick or the buffer
//!    reaching [`FLUSH_BYTES`]. This keeps a chatty session from flooding
//!    coord with one HTTP POST per keystroke-echo.
//! 5. POSTs each coalesced chunk to coord
//!    `POST /sessions/:id/output {chunk_offset, payload_b64}`. The
//!    `chunk_offset` is a monotonic per-session byte counter, giving the
//!    warm tier a stable FIFO order + idempotency key. coord takes the
//!    row's tenant from the SESSION ROW (`coord.sessions.tenant_id`), never
//!    from the bearer, so the runner sends no tenant in the payload. The
//!    bearer is the owning session's device-JWT slot, and it is what coord
//!    checks OWNERSHIP against (plan
//!    `2026-09-28-anyone-holding-a-session-uuid-can-write-its-transcript-because-session-output-and-event-writes-are-anonymous`):
//!    with no live credential the flush sends nothing at all rather than an
//!    anonymous request (see [`flush`]).
//!
//! ## Transport coupling
//!
//! The tap is reached via [`crate::session::transport::Transport::tap_output`],
//! implemented by every transport whose handle names a real PTY. Both
//! `TerminalShell` (the PTY transport) and `TerminalClaude` (the claude_cli
//! transport) stream: their `start` returns the same
//! [`crate::session::transport::TransportHandle::Pty`] and both tap it through
//! the shared `transport::tap_pty_output`, so there is one code path and one
//! set of guarantees for both.
//!
//! Two arms still return `None`, and the reasons are recorded at each impl
//! rather than here: the `Agentic` arm of the claude_cli transport and the
//! whole workflow transport. Both stamp a `pending-<uuid>` placeholder handle
//! that names no live process — `SessionRegistry::link_task_run`, the linker
//! their docs name, does not exist and
//! `SessionRecord::transport_handle` is never mutated after
//! `SessionRegistry::start_inner` writes it — and a workflow run has no byte
//! stream at all (it is step-driven; its output is structured events). For
//! those the pipe is simply never spawned.
//!
//! Everything downstream of the tap is transport-agnostic: `run_pipe` below
//! applies [`super::redact::redact_secrets`] and the coalescing / rate-limit
//! loop to whatever receiver it is handed, and `SessionRegistry::start_inner`
//! is the single site that pairs a tap with
//! [`Intent::effective_redact_secrets`]. A newly wired transport therefore
//! inherits redaction and coalescing by construction, with nothing to opt in
//! to.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::auth::TenantScope;

use super::coord_sync::CoordSync;
use super::redact::redact_secrets;

/// Flush the coalescing buffer at least this often, even if it hasn't hit
/// the byte threshold. Keeps live-tail latency low (sub-100ms) while still
/// batching bursts. Plan §Phase 8 "rate-limited".
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);

/// Flush early once the buffer reaches this many bytes, so a burst doesn't
/// wait the full interval. 16 KiB comfortably under coord's 1 MiB
/// per-chunk sanity bound.
const FLUSH_BYTES: usize = 16 * 1024;

/// Drop (and warn once) if a single coalesced flush would exceed this —
/// the runner should never build a chunk anywhere near coord's 1 MiB
/// ceiling, but bound it defensively so a runaway producer can't OOM the
/// buffer.
const MAX_BUFFER_BYTES: usize = 512 * 1024;

// Secret redaction moved to the shared [`super::redact`] module (plan
// `2026-07-09-runner-session-history-cloud-sync` Phase 3) so the transcript
// emitter and this pipe run the exact same sweep. Tests live there too.

/// Spawn the output pipe for a session. Returns the [`JoinHandle`] so the
/// registry can keep it alive for the session's lifetime (dropping it
/// aborts the task; the registry holds it in the session record).
///
/// `redact` is the resolved [`Intent::effective_redact_secrets`] value.
/// `rx` is the transport's output broadcast receiver.
///
/// `tenant` is the OWNING session's scope, supplied by the caller. It is not
/// resolved here: this pipe can start BEFORE the session record is inserted, so
/// a self-resolving version would answer `Unresolved` for early chunks and (on
/// a multi-bound device) find no credential to send them under for no reason.
/// It selects WHICH device-JWT slot is presented; coord does not derive the
/// row's tenant from that bearer (it reads the session row) but checks the
/// bearer against the row's owning device and tenant, so a wrong `Owned`
/// presents a credential coord will refuse.
pub fn spawn(
    coord_sync: CoordSync,
    session_id: Uuid,
    rx: broadcast::Receiver<String>,
    redact: bool,
    tenant: TenantScope,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_pipe(coord_sync, session_id, rx, redact, tenant))
}

/// The pipe loop. Coalesces decoded (+ optionally redacted) output and
/// flushes to coord on timer or byte threshold. Exits when the broadcast
/// sender is dropped (terminal closed) and the buffer is drained.
async fn run_pipe(
    coord_sync: CoordSync,
    session_id: Uuid,
    mut rx: broadcast::Receiver<String>,
    redact: bool,
    tenant: TenantScope,
) {
    let http = coord_sync.http_client();
    let base = coord_sync.coord_url().trim_end_matches('/').to_string();
    let url = format!("{base}/sessions/{session_id}/output");

    tracing::info!(
        session = %session_id,
        redact,
        "session output_pipe: streaming enabled"
    );

    let mut buffer: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
    // Monotonic per-session byte offset of the first byte in the NEXT
    // chunk we POST. coord uses (session_id, chunk_offset) as the warm-tier
    // PK so a retry of the same offset is idempotent.
    let mut next_offset: i64 = 0;
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // New output chunk (base64 string) from the transport.
            recv = rx.recv() => {
                match recv {
                    Ok(encoded) => {
                        match base64::engine::general_purpose::STANDARD
                            .decode(encoded.as_bytes())
                        {
                            Ok(raw) => {
                                let processed = if redact {
                                    redact_secrets(&raw)
                                } else {
                                    raw
                                };
                                buffer.extend_from_slice(&processed);
                                if buffer.len() >= FLUSH_BYTES {
                                    flush(
                                        &http, &url, session_id, &mut buffer,
                                        &mut next_offset, tenant,
                                    )
                                    .await;
                                }
                                if buffer.len() > MAX_BUFFER_BYTES {
                                    // Should never happen (we flush at
                                    // FLUSH_BYTES) but bound defensively.
                                    tracing::warn!(
                                        session = %session_id,
                                        len = buffer.len(),
                                        "session output_pipe: buffer over hard cap — force flush"
                                    );
                                    flush(
                                        &http, &url, session_id, &mut buffer,
                                        &mut next_offset, tenant,
                                    )
                                    .await;
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    session = %session_id,
                                    error = %e,
                                    "session output_pipe: base64 decode failed — skipping chunk"
                                );
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // The broadcast channel dropped chunks because the
                        // pipe fell behind the producer. Log + continue —
                        // a shared tail tolerating gaps is acceptable
                        // (the operator's own view is the source of truth;
                        // this is a courtesy mirror).
                        tracing::warn!(
                            session = %session_id,
                            skipped,
                            "session output_pipe: lagged — dropped output chunks"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Terminal closed. Final flush + exit.
                        flush(
                            &http, &url, session_id, &mut buffer, &mut next_offset,
                            tenant,
                        )
                        .await;
                        tracing::info!(
                            session = %session_id,
                            "session output_pipe: transport closed — pipe exiting"
                        );
                        return;
                    }
                }
            }
            // Timer tick — flush whatever's buffered so live-tail latency
            // stays low even on a trickle of output.
            _ = ticker.tick() => {
                if !buffer.is_empty() {
                    flush(
                        &http, &url, session_id, &mut buffer, &mut next_offset,
                        tenant,
                    )
                    .await;
                }
            }
        }
    }
}

/// POST the buffered bytes as one coalesced chunk + advance the offset.
/// Best-effort: a coord-side failure (network, 401, 429 quota, 5xx) is logged
/// and the buffer is cleared — output streaming is a courtesy mirror, so
/// we never block the operator's session or retry-storm coord. A 429
/// (tenant warm quota exceeded) is logged at info and treated as
/// "stop trying for now"; the next flush simply tries again on fresh
/// output (coord re-checks the quota each call).
///
/// **No live credential, no request.** The route admits only the owning
/// device's credential, so when [`crate::auth::try_attach_device_auth_for`]
/// resolves none the chunk is dropped WITHOUT a POST — the same loss this lossy
/// path already takes on a 429 or 5xx, minus an anonymous request. The durable
/// transcript lane (the outbox) holds instead; this one never did.
async fn flush(
    http: &reqwest::Client,
    url: &str,
    session_id: Uuid,
    buffer: &mut Vec<u8>,
    next_offset: &mut i64,
    tenant: TenantScope,
) {
    if buffer.is_empty() {
        return;
    }
    if super::coord_sync::transcript_sync_refused(session_id) {
        // The tenant turned transcript sync off (coord 429
        // `transcript_sync_disabled`, which covers every stream).
        buffer.clear();
        return;
    }
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&buffer[..]);
    let offset = *next_offset;
    let len = buffer.len() as i64;
    let body = serde_json::json!({
        "chunk_offset": offset,
        "payload_b64": payload_b64,
    });

    let rb = match crate::auth::try_attach_device_auth_for(http.post(url).json(&body), tenant) {
        Ok(rb) => rb,
        Err(cause) => {
            tracing::debug!(
                session = %session_id,
                %cause,
                "session output_pipe: no live device credential — dropping chunk unsent"
            );
            buffer.clear();
            return;
        }
    };
    match rb.send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                // Advance the offset only on a confirmed store so a
                // transient failure re-sends the same offset (idempotent
                // on coord). On success, the bytes are durably in warm.
                *next_offset += len;
            } else if status == reqwest::StatusCode::UNAUTHORIZED {
                // Coord refused the credential presented. Dropped like a 429
                // or 5xx — this path is lossy by design; the durable outbox
                // lane holds its rows instead.
                tracing::debug!(
                    session = %session_id,
                    "session output_pipe: coord refused the device credential (401) — \
                     dropping chunk"
                );
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let detail = resp.text().await.unwrap_or_default();
                if super::coord_sync::is_tenant_transcript_refusal(&detail) {
                    // Tenant consent is off for every stream: stop asking for
                    // this session until the next start, like the outbox lane.
                    // (A `column_missing: true` refusal is coord's
                    // deploy-ordering fallback, not the tenant's — it drops
                    // this chunk below and asks again on the next flush.)
                    if super::coord_sync::note_transcript_sync_refused(session_id) {
                        tracing::info!(
                            session = %session_id,
                            "session output_pipe: the tenant has transcript sync turned off \
                             — no more output for this session until the next start"
                        );
                    }
                } else {
                    tracing::info!(
                        session = %session_id,
                        "session output_pipe: coord paced this chunk (429: warm quota, or a \
                         transcript-consent fallback while coord's migration lands) — dropping it"
                    );
                }
                // Drop the chunk (don't advance offset — but also don't
                // resend; the bytes are gone for the shared tail). Clearing
                // below handles it.
            } else {
                tracing::debug!(
                    session = %session_id,
                    %status,
                    "session output_pipe: coord rejected chunk — dropping"
                );
            }
        }
        Err(e) => {
            tracing::debug!(
                session = %session_id,
                error = %e,
                "session output_pipe: POST failed — dropping chunk (courtesy mirror)"
            );
        }
    }
    buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake coord `POST /sessions/:id/output` that counts requests and
    /// records whether each carried a bearer.
    async fn fake_output_route() -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<bool>>>) {
        use axum::{routing::post, Router};
        let hits = Arc::new(AtomicUsize::new(0));
        let authed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (h, a) = (hits.clone(), authed.clone());
        let app = Router::new().route(
            "/sessions/{id}/output",
            post(move |headers: axum::http::HeaderMap| {
                let (h, a) = (h.clone(), a.clone());
                async move {
                    h.fetch_add(1, Ordering::SeqCst);
                    a.lock()
                        .unwrap()
                        .push(headers.contains_key("authorization"));
                    axum::Json(serde_json::json!({"stored": true}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), hits, authed)
    }

    /// A device JWT of the shape `slot_jwt_is_usable` accepts (live `exp`).
    fn live_device_jwt() -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"none","typ":"JWT"}"#);
        let exp = chrono::Utc::now().timestamp() + 3 * 60 * 60;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"tenant_id":"{}","exp":{exp}}}"#, Uuid::now_v7()).as_bytes());
        format!("{header}.{payload}.sig")
    }

    /// The lossy PTY path never sends an ANONYMOUS request: with no live device
    /// credential the chunk is dropped unsent (buffer cleared, offset kept),
    /// and with one it is POSTed carrying the bearer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pty_flush_sends_nothing_without_a_live_credential() {
        let _amb = crate::test_env::isolated_ambient();
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        let (base, hits, authed) = fake_output_route().await;
        let http = reqwest::Client::new();
        let session = Uuid::new_v4();
        let url = format!("{base}/sessions/{session}/output");

        let mut buffer = b"hello".to_vec();
        let mut next_offset = 0i64;
        flush(
            &http,
            &url,
            session,
            &mut buffer,
            &mut next_offset,
            TenantScope::Unresolved,
        )
        .await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "no live credential → no request at all, never an anonymous one"
        );
        assert!(buffer.is_empty(), "the chunk is dropped (lossy by design)");
        assert_eq!(
            next_offset, 0,
            "a dropped chunk does not advance the offset"
        );

        crate::auth::AuthManager::new()
            .store_tokens(&live_device_jwt(), "")
            .unwrap();
        let mut buffer = b"hello".to_vec();
        flush(
            &http,
            &url,
            session,
            &mut buffer,
            &mut next_offset,
            TenantScope::Unresolved,
        )
        .await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(*authed.lock().unwrap(), vec![true], "sent WITH the bearer");
        assert_eq!(next_offset, 5);
    }
}
