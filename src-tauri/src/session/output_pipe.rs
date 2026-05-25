//! PTY output pipe — taps a session's terminal output and publishes it to
//! coord's warm/hot/cold retention tiers. Plan
//! [`2026-05-23-coord-native-sessions-phase-7-10`] §Phase 8 (plan §D10 +
//! §D11).
//!
//! ## When it runs
//!
//! Only when the session's [`Intent::share_output`] is `true`. Off by
//! default: a session that didn't opt in pays **zero overhead** — no task
//! is spawned, no receiver is subscribed, no bytes leave the machine.
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
//!    warm tier a stable FIFO order + idempotency key. coord resolves the
//!    session's tenant server-side, so the runner sends no tenant here.
//!
//! ## Transport coupling
//!
//! The tap is reached via [`crate::session::transport::Transport::tap_output`]
//! (default `None`; implemented for the PTY transport). Today only
//! TerminalShell sessions stream; claude_cli / workflow return `None` and
//! the pipe is simply never spawned for them. Wiring those transports'
//! output is a follow-up.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use once_cell::sync::Lazy;
use regex::Regex;
use tokio::sync::broadcast;
use uuid::Uuid;

use super::coord_sync::CoordSync;

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

/// Secret-redaction regex (plan §D11). Masks `key = value` / `key: value`
/// fragments whose key looks like a credential. Case-insensitive. The
/// value (everything up to the next whitespace) is replaced with a fixed
/// mask. Compiled once.
///
/// This is intentionally narrow + fast — it's a courtesy backstop, not a
/// DLP engine. Documented as defense-in-depth per the plan.
static SECRET_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(api[_-]?key|password|token|secret)(\s*[=:]\s*)(\S+)")
        .expect("secret redaction regex is a valid literal")
});

/// Mask substituted for a matched secret value.
const MASK: &str = "***REDACTED***";

/// Apply the redaction sweep to a byte slice. Operates on the UTF-8 lossy
/// view (PTY output is overwhelmingly UTF-8; non-UTF-8 bytes become the
/// replacement char in the masked output, which is acceptable for a
/// shared-tail courtesy view). Returns the (possibly) masked bytes.
///
/// Public so the unit tests + any future caller share the one
/// implementation.
pub fn redact_secrets(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let masked = SECRET_RE.replace_all(&text, |caps: &regex::Captures| {
        // Keep the key + separator, mask only the value.
        format!("{}{}{}", &caps[1], &caps[2], MASK)
    });
    masked.into_owned().into_bytes()
}

/// Spawn the output pipe for a session. Returns the [`JoinHandle`] so the
/// registry can keep it alive for the session's lifetime (dropping it
/// aborts the task; the registry holds it in the session record).
///
/// `redact` is the resolved [`Intent::effective_redact_secrets`] value.
/// `rx` is the transport's output broadcast receiver.
pub fn spawn(
    coord_sync: CoordSync,
    session_id: Uuid,
    rx: broadcast::Receiver<String>,
    redact: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_pipe(coord_sync, session_id, rx, redact))
}

/// The pipe loop. Coalesces decoded (+ optionally redacted) output and
/// flushes to coord on timer or byte threshold. Exits when the broadcast
/// sender is dropped (terminal closed) and the buffer is drained.
async fn run_pipe(
    coord_sync: CoordSync,
    session_id: Uuid,
    mut rx: broadcast::Receiver<String>,
    redact: bool,
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
                                    flush(&http, &url, session_id, &mut buffer, &mut next_offset)
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
                                    flush(&http, &url, session_id, &mut buffer, &mut next_offset)
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
                        flush(&http, &url, session_id, &mut buffer, &mut next_offset).await;
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
                    flush(&http, &url, session_id, &mut buffer, &mut next_offset).await;
                }
            }
        }
    }
}

/// POST the buffered bytes as one coalesced chunk + advance the offset.
/// Best-effort: a coord-side failure (network, 429 quota, 5xx) is logged
/// and the buffer is cleared — output streaming is a courtesy mirror, so
/// we never block the operator's session or retry-storm coord. A 429
/// (tenant warm quota exceeded) is logged at info and treated as
/// "stop trying for now"; the next flush simply tries again on fresh
/// output (coord re-checks the quota each call).
async fn flush(
    http: &reqwest::Client,
    url: &str,
    session_id: Uuid,
    buffer: &mut Vec<u8>,
    next_offset: &mut i64,
) {
    if buffer.is_empty() {
        return;
    }
    let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&buffer[..]);
    let offset = *next_offset;
    let len = buffer.len() as i64;
    let body = serde_json::json!({
        "chunk_offset": offset,
        "payload_b64": payload_b64,
    });

    match http.post(url).json(&body).send().await {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                // Advance the offset only on a confirmed store so a
                // transient failure re-sends the same offset (idempotent
                // on coord). On success, the bytes are durably in warm.
                *next_offset += len;
            } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tracing::info!(
                    session = %session_id,
                    "session output_pipe: coord warm quota exceeded — dropping chunk"
                );
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

    #[test]
    fn redacts_key_value_secrets() {
        let input = b"export API_KEY=sk-abc123def\nnormal line\npassword: hunter2";
        let out = String::from_utf8(redact_secrets(input)).unwrap();
        assert!(out.contains("API_KEY=***REDACTED***"), "got: {out}");
        assert!(out.contains("password: ***REDACTED***"), "got: {out}");
        assert!(out.contains("normal line"));
        // The literal secret values must be gone.
        assert!(!out.contains("sk-abc123def"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn redacts_token_and_secret_variants() {
        for (line, secret) in [
            ("TOKEN=ghp_xxx", "ghp_xxx"),
            ("client_secret = abcdef", "abcdef"),
            ("api-key: zzz", "zzz"),
        ] {
            let out = String::from_utf8(redact_secrets(line.as_bytes())).unwrap();
            assert!(out.contains(MASK), "expected mask in: {out}");
            assert!(!out.contains(secret), "secret leaked in: {out}");
        }
    }

    #[test]
    fn leaves_non_secret_output_untouched() {
        let input = b"ls -la\ntotal 42\ndrwxr-xr-x  3 user group";
        let out = redact_secrets(input);
        assert_eq!(out, input);
    }

    #[test]
    fn redaction_is_lossless_for_keys() {
        // The key + separator survive; only the value is masked. This keeps
        // the shared tail legible ("there was an API_KEY here") without
        // leaking the value.
        let out = String::from_utf8(redact_secrets(b"API_KEY=secret123")).unwrap();
        assert_eq!(out, "API_KEY=***REDACTED***");
    }
}
