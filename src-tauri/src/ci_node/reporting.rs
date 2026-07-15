//! Progress streaming + result POSTs for CI dispatches.
//!
//! Wire contract (pinned):
//! - `POST {coord}/coord/ci/dispatches/{dispatch_id}/progress` with
//!   `{"lines": ["..."], "progress_seq": N}` — batched (2s / 50 lines),
//!   `progress_seq` strictly monotonic per dispatch.
//! - `POST {coord}/coord/ci/dispatches/{dispatch_id}/result` with
//!   `{"conclusion", "summary": {"steps": [...]}, "log_tail"}` — retried
//!   with backoff (the same never-drop-the-verdict discipline as the agent
//!   log POST retry queue; coord's lease sweeper covers the truly-lost case).
//!
//! Auth posture: **device-JWT bearer on every POST** (plan §4.3). Coord
//! mounts both routes behind `require_jwt` and 403s any caller whose JWT
//! `device_id`/`tenant_id` claims don't match the dispatch row's assignee
//! (`ci_dispatch::authorize_caller`) — an unauthenticated POST is a
//! guaranteed 401, so the bearer is load-bearing, not optional. The token is
//! resolved fresh per POST from the same slot the device-JWT refresher keeps
//! warm (`coord_mcp::read_usable_device_jwt`, with a bounded kick-and-wait
//! fallback), so a multi-hour build outliving one 4h token still reports.
//!
//! Both routes answer `{"state": <ledger state>}`. A TERMINAL state on a
//! progress response (`lost` after a lease expiry, `cancelled` after a
//! sweep) means coord no longer wants this build — the flusher cancels the
//! dispatch's token so the executor stops burning the machine.

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Flush when this many lines are pending…
pub(crate) const FLUSH_MAX_LINES: usize = 50;
/// …or when this much time has passed since the last flush.
pub(crate) const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
/// Bound on lines buffered while coord is unreachable (drop-oldest).
const RETRY_QUEUE_CAP_LINES: usize = 5_000;
/// Log-tail ring budget (~32 KB, per the contract).
pub(crate) const LOG_TAIL_CAP_BYTES: usize = 32 * 1024;
/// Result POST retry schedule (seconds).
const RESULT_RETRY_SECS: &[u64] = &[2, 4, 8, 16, 32];

/// Decide whether a pending batch should flush. Pure, unit-tested.
pub(crate) fn should_flush(pending_lines: usize, elapsed_since_last: Duration) -> bool {
    pending_lines > 0 && (pending_lines >= FLUSH_MAX_LINES || elapsed_since_last >= FLUSH_INTERVAL)
}

/// Coord's terminal dispatch-ledger states (mirrors `ci_dispatch::is_terminal`
/// on the coord side — pinned wire contract). A progress response carrying one
/// of these means coord will accept no further writes for this dispatch.
pub(crate) fn is_terminal_dispatch_state(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "lost" | "cancelled")
}

/// Extract `{"state": ...}` from an ingest-route response body.
fn parse_response_state(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("state")?
        .as_str()
        .map(|s| s.to_string())
}

/// Resolve the device-JWT bearer for one ingest POST: the cheap fresh-read
/// first, then the bounded kick-and-wait remint. `None` (rare — an unpaired
/// or auth-wedged device) makes the caller treat the POST as failed so the
/// retry machinery keeps the payload.
async fn device_bearer() -> Option<String> {
    if let Some(t) = crate::coord_mcp::read_usable_device_jwt().await {
        return Some(t);
    }
    crate::coord_mcp::await_device_jwt_remint().await
}

/// What to do after one result-POST attempt. Pure over the observed status
/// so the policy is unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PostDisposition {
    /// 2xx — coord recorded it.
    Done,
    /// 409 `dispatch_terminal`: coord already holds a terminal state for
    /// this dispatch (a sweep marked it `lost`, or a duplicate with a
    /// different conclusion). The ledger stands; retrying can never succeed.
    TerminalConflict,
    /// A non-retryable client error (400 bad conclusion, 403 not assignee,
    /// 404 unknown dispatch) — re-sending the same body cannot heal it.
    GiveUp,
    /// Network failure, 5xx, or an auth-refresh-shaped status (401/408/429)
    /// — retry on the schedule.
    Retry,
}

/// Classify a result-POST outcome. `None` = no HTTP status (network error).
pub(crate) fn result_post_disposition(status: Option<u16>) -> PostDisposition {
    match status {
        Some(s) if (200..300).contains(&s) => PostDisposition::Done,
        Some(409) => PostDisposition::TerminalConflict,
        // 401 can be a token freshly reminted mid-flight; 408/429 are
        // explicitly transient. Everything else in 4xx is a contract error
        // that a byte-identical retry cannot fix.
        Some(401) | Some(408) | Some(429) => PostDisposition::Retry,
        Some(s) if (400..500).contains(&s) => PostDisposition::GiveUp,
        _ => PostDisposition::Retry,
    }
}

/// Bounded byte-ring of the newest log lines (the result POST's `log_tail`).
pub(crate) struct TailRing {
    lines: VecDeque<String>,
    bytes: usize,
    cap: usize,
}

impl TailRing {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            cap,
        }
    }

    pub(crate) fn push(&mut self, line: &str) {
        let cost = line.len() + 1; // +1 for the joining newline
        self.lines.push_back(line.to_string());
        self.bytes += cost;
        while self.bytes > self.cap {
            match self.lines.pop_front() {
                Some(dropped) => self.bytes -= dropped.len() + 1,
                None => break,
            }
        }
    }

    pub(crate) fn contents(&self) -> String {
        let mut out = String::with_capacity(self.bytes);
        for line in &self.lines {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

#[derive(Serialize)]
struct ProgressBody<'a> {
    lines: &'a [String],
    progress_seq: u64,
}

/// One step's row in the result summary.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StepSummary {
    pub name: String,
    /// `success` | `failure` | `cancelled`.
    pub conclusion: String,
    pub duration_secs: u64,
}

/// Streaming sink for one dispatch: `push()` lines from any task; a
/// dedicated flusher batches them to the progress route with a monotonic
/// `progress_seq` and an on-failure retry queue. `finish()` closes the
/// stream, drains what it can, and hands back the log tail.
pub(crate) struct ProgressSink {
    tx: mpsc::UnboundedSender<String>,
    tail: Arc<Mutex<TailRing>>,
    flusher: tokio::task::JoinHandle<()>,
}

impl ProgressSink {
    /// `cancel` is the dispatch's cancellation token: when a progress
    /// response reports a TERMINAL ledger state (coord swept the lease or an
    /// operator cancelled), the flusher cancels it so the executor stops.
    pub(crate) fn start(
        coord_base: String,
        dispatch_id: String,
        cancel: CancellationToken,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        let tail = Arc::new(Mutex::new(TailRing::new(LOG_TAIL_CAP_BYTES)));
        let flusher = tokio::spawn(flusher_loop(coord_base, dispatch_id, rx, cancel));
        Self { tx, tail, flusher }
    }

    /// Record one output line (tail ring + progress batch). Never blocks.
    pub(crate) fn push(&self, line: &str) {
        if let Ok(mut tail) = self.tail.lock() {
            tail.push(line);
        }
        // A closed channel means finish() already ran — tail-only is fine.
        let _ = self.tx.send(line.to_string());
    }

    /// A cloneable line-pusher for the stdout/stderr pump tasks.
    pub(crate) fn pusher(&self) -> LinePusher {
        LinePusher {
            tx: self.tx.clone(),
            tail: self.tail.clone(),
        }
    }

    /// Close the stream, wait for the flusher to drain, return the tail.
    pub(crate) async fn finish(self) -> String {
        drop(self.tx);
        if let Err(e) = self.flusher.await {
            warn!("ci_node: progress flusher join failed: {e}");
        }
        self.tail.lock().map(|t| t.contents()).unwrap_or_default()
    }
}

/// Clone-able handle for pump tasks (same behavior as [`ProgressSink::push`]).
#[derive(Clone)]
pub(crate) struct LinePusher {
    tx: mpsc::UnboundedSender<String>,
    tail: Arc<Mutex<TailRing>>,
}

impl LinePusher {
    pub(crate) fn push(&self, line: &str) {
        if let Ok(mut tail) = self.tail.lock() {
            tail.push(line);
        }
        let _ = self.tx.send(line.to_string());
    }
}

/// Flusher: accumulate lines; POST a batch every [`FLUSH_INTERVAL`] or
/// [`FLUSH_MAX_LINES`]; on POST failure keep the lines queued (bounded,
/// drop-oldest) and retry on the next tick — the `forward_stream` retry-
/// queue discipline. Exits after the channel closes and a final drain pass.
async fn flusher_loop(
    coord_base: String,
    dispatch_id: String,
    mut rx: mpsc::UnboundedReceiver<String>,
    cancel: CancellationToken,
) {
    let url = format!(
        "{}/coord/ci/dispatches/{}/progress",
        coord_base.trim_end_matches('/'),
        dispatch_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .ok();

    /// One authenticated batch POST. `true` = coord accepted the batch
    /// (drop it from the queue) — including the terminal-state case, where
    /// coord accepted nothing but never will (re-sending is pointless), so
    /// the token is cancelled and the lines are surrendered.
    async fn post_batch(
        client: &Option<reqwest::Client>,
        url: &str,
        batch: &[String],
        seq: u64,
        dispatch_id: &str,
        cancel: &CancellationToken,
    ) -> bool {
        let Some(client) = client else { return false };
        let Some(bearer) = device_bearer().await else {
            debug!("ci_node: no device JWT for progress POST (dispatch {dispatch_id})");
            return false;
        };
        let body = ProgressBody {
            lines: batch,
            progress_seq: seq,
        };
        match client
            .post(url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(text) = resp.text().await {
                    if let Some(state) = parse_response_state(&text) {
                        if is_terminal_dispatch_state(&state) && !cancel.is_cancelled() {
                            warn!(
                                "ci_node: coord reports dispatch {dispatch_id} already terminal \
                                 ({state}) — cancelling the local build"
                            );
                            cancel.cancel();
                        }
                    }
                }
                true
            }
            Ok(resp) => {
                debug!(
                    "ci_node: progress POST for dispatch {dispatch_id} returned {}",
                    resp.status()
                );
                false
            }
            Err(e) => {
                debug!("ci_node: progress POST for dispatch {dispatch_id} failed: {e}");
                false
            }
        }
    }
    let mut pending: VecDeque<String> = VecDeque::new();
    let mut seq: u64 = 0;
    let mut last_flush = tokio::time::Instant::now();
    let mut open = true;

    while open || !pending.is_empty() {
        if open {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(line) => {
                        pending.push_back(line);
                        while pending.len() > RETRY_QUEUE_CAP_LINES {
                            pending.pop_front();
                        }
                    }
                    None => open = false,
                },
                _ = tokio::time::sleep_until(last_flush + FLUSH_INTERVAL) => {
                    if pending.is_empty() {
                        // Idle tick with nothing to send: advance the timer
                        // baseline so this branch doesn't hot-loop.
                        last_flush = tokio::time::Instant::now();
                    }
                }
            }
        }
        let closing = !open;
        if should_flush(pending.len(), last_flush.elapsed()) || (closing && !pending.is_empty()) {
            // Up to 500 lines per POST — bigger than the 50-line trigger so
            // a backlog built up during a coord blip drains quickly.
            let batch: Vec<String> = pending.iter().take(500).cloned().collect();
            seq += 1;
            let ok = post_batch(&client, &url, &batch, seq, &dispatch_id, &cancel).await;
            last_flush = tokio::time::Instant::now();
            if ok {
                for _ in 0..batch.len() {
                    pending.pop_front();
                }
            } else {
                debug!(
                    "ci_node: progress POST failed (queued {} lines)",
                    pending.len()
                );
                if closing {
                    // Final drain: one bounded retry round, then give up —
                    // the result POST's log_tail still carries the content.
                    // The retry re-sends the same seq (duplicates tolerated;
                    // seq is informational on coord's side).
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if post_batch(&client, &url, &batch, seq, &dispatch_id, &cancel).await {
                        for _ in 0..batch.len() {
                            pending.pop_front();
                        }
                    } else {
                        warn!(
                            "ci_node: dropping {} undeliverable progress lines for dispatch {} \
                             (content preserved in log_tail)",
                            pending.len(),
                            dispatch_id
                        );
                        pending.clear();
                    }
                }
            }
        }
    }
}

/// POST the final result, retrying on the [`RESULT_RETRY_SECS`] schedule.
/// `summary_extra_reason` adds a `reason` key next to `steps` (used for
/// admission rejections).
pub(crate) async fn post_result(
    coord_base: &str,
    dispatch_id: &str,
    conclusion: &str,
    steps: &[StepSummary],
    reason: Option<&str>,
    log_tail: &str,
) {
    let url = format!(
        "{}/coord/ci/dispatches/{}/result",
        coord_base.trim_end_matches('/'),
        dispatch_id
    );
    let mut summary = serde_json::json!({ "steps": steps });
    if let Some(r) = reason {
        summary["reason"] = serde_json::Value::String(r.to_string());
    }
    let body = serde_json::json!({
        "conclusion": conclusion,
        "summary": summary,
        "log_tail": log_tail,
    });
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    else {
        warn!("ci_node: reqwest client build failed; result POST skipped");
        return;
    };
    let mut attempts = 0usize;
    loop {
        let status = match device_bearer().await {
            Some(bearer) => client
                .post(&url)
                .bearer_auth(bearer)
                .json(&body)
                .send()
                .await
                .ok()
                .map(|r| r.status().as_u16()),
            None => {
                warn!("ci_node: no device JWT for result POST (dispatch {dispatch_id})");
                None
            }
        };
        match result_post_disposition(status) {
            PostDisposition::Done => {
                debug!("ci_node: result POST ok dispatch={dispatch_id} conclusion={conclusion}");
                return;
            }
            PostDisposition::TerminalConflict => {
                // Coord already holds a terminal state (e.g. the lease
                // sweeper marked it `lost` while we were finishing). The
                // ledger stands — a late result is coord's problem, not a
                // retry loop's (plan §4.5).
                warn!(
                    "ci_node: result POST for dispatch {dispatch_id} conflicted (409) — \
                     coord already recorded a terminal state; dropping"
                );
                return;
            }
            PostDisposition::GiveUp => {
                warn!(
                    "ci_node: result POST for dispatch {dispatch_id} rejected \
                     (status {status:?}) — non-retryable; dropping"
                );
                return;
            }
            PostDisposition::Retry => {}
        }
        if attempts >= RESULT_RETRY_SECS.len() {
            warn!(
                "ci_node: result POST failed after {} attempts for dispatch {} — \
                 coord's dispatch-lease sweeper will mark it lost",
                attempts + 1,
                dispatch_id
            );
            return;
        }
        let delay = RESULT_RETRY_SECS[attempts];
        warn!(
            "ci_node: result POST failed (attempt {}, status {status:?}); retrying in {delay}s",
            attempts + 1
        );
        tokio::time::sleep(Duration::from_secs(delay)).await;
        attempts += 1;
    }
}

/// Fire-and-forget a `cancelled` result with a reason and no steps — the
/// admission-rejection path (hard reject / disk floor / cancelled-while-
/// queued).
pub(crate) fn post_cancelled_result_detached(
    coord_base: String,
    dispatch_id: String,
    reason: String,
) {
    tokio::spawn(async move {
        post_result(
            &coord_base,
            &dispatch_id,
            "cancelled",
            &[],
            Some(&reason),
            "",
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_ring_keeps_newest_within_budget() {
        let mut ring = TailRing::new(64);
        for i in 0..100 {
            ring.push(&format!("line-{i:03}"));
        }
        let contents = ring.contents();
        assert!(contents.len() <= 64 + 9, "must stay near budget");
        assert!(contents.contains("line-099"), "newest line must survive");
        assert!(!contents.contains("line-000"), "oldest must be evicted");
    }

    #[test]
    fn tail_ring_single_oversize_line_still_bounded() {
        let mut ring = TailRing::new(16);
        ring.push(&"x".repeat(100));
        // One oversized line: ring holds at most that line, and the next
        // push evicts it.
        ring.push("short");
        let contents = ring.contents();
        assert_eq!(contents, "short\n");
    }

    #[test]
    fn flush_trigger_logic() {
        assert!(
            !should_flush(0, Duration::from_secs(60)),
            "no lines → no flush"
        );
        assert!(
            !should_flush(1, Duration::from_millis(100)),
            "young + few → wait"
        );
        assert!(
            should_flush(FLUSH_MAX_LINES, Duration::from_millis(0)),
            "batch full → flush"
        );
        assert!(should_flush(1, FLUSH_INTERVAL), "interval elapsed → flush");
    }

    #[test]
    fn terminal_dispatch_states_pinned_to_coord_contract() {
        // Mirrors coord ci_dispatch::is_terminal exactly.
        for s in ["succeeded", "failed", "lost", "cancelled"] {
            assert!(is_terminal_dispatch_state(s), "{s} must be terminal");
        }
        for s in ["queued", "dispatched", "running", ""] {
            assert!(!is_terminal_dispatch_state(s), "{s} must be live");
        }
    }

    #[test]
    fn response_state_parses_ingest_shape() {
        assert_eq!(
            parse_response_state(r#"{"state":"running"}"#),
            Some("running".to_string())
        );
        assert_eq!(parse_response_state(r#"{"error":"x"}"#), None);
        assert_eq!(parse_response_state("not json"), None);
    }

    #[test]
    fn result_disposition_policy() {
        assert_eq!(result_post_disposition(Some(200)), PostDisposition::Done);
        assert_eq!(
            result_post_disposition(Some(409)),
            PostDisposition::TerminalConflict
        );
        // Contract errors: retrying an identical body cannot heal these.
        for s in [400, 403, 404] {
            assert_eq!(result_post_disposition(Some(s)), PostDisposition::GiveUp);
        }
        // Transient shapes retry.
        for s in [401, 408, 429, 500, 503] {
            assert_eq!(result_post_disposition(Some(s)), PostDisposition::Retry);
        }
        assert_eq!(result_post_disposition(None), PostDisposition::Retry);
    }

    #[test]
    fn step_summary_wire_shape() {
        let s = StepSummary {
            name: "rust-test".into(),
            conclusion: "success".into(),
            duration_secs: 42,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"name": "rust-test", "conclusion": "success", "duration_secs": 42})
        );
    }
}
