//! Retry a worktree acquire's coord calls through intermittent coord failures
//! (plan
//! `2026-10-31-continuation-worktree-acquire-failure-strands-the-allocation-and-tells-the-session-nothing`
//! Phase 1).
//!
//! The acquire has ONE deadline, computed once before the allocate and shared
//! by the allocate retry and every claim taken after it (D3). It is not a
//! per-call budget: the claims already held have a 300 s TTL and their
//! heartbeats start only after the acquire returns, so a per-claim budget of
//! 120 s over three rows could let the first claim expire.
//!
//! A zero budget ([`RetryDeadline::none`]) makes exactly one attempt and keeps
//! today's behaviour. Only the unattended continuation path sets a budget
//! ([`CONTINUATION_TRANSPORT_RETRY`]): an interactive spawn has a user waiting.
//!
//! What is retried is decided on typed outcomes ([`Attempt::Transient`]), never
//! on error text: a transport error, a body-read failure and a 5xx. A claim
//! `held`, a `repo_not_registered`, a `wait`, any other 4xx, a parse failure of
//! a fully read body and every `git` failure are final.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

use tracing::info;

/// The continuation path's acquire budget (D3). Set by the claim TTL, not by
/// outage length: it leaves 180 s of the 300 s claim TTL for materialization
/// before the first heartbeat.
pub const CONTINUATION_TRANSPORT_RETRY: Duration = Duration::from_secs(120);

/// No single coord request may outlive this, whatever budget remains: a hung
/// send never returns and would otherwise escape the deadline.
pub(crate) const PER_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Floor for an attempt's timeout once the remaining budget is nearly spent,
/// so a last attempt is not sent with a timeout too short to answer.
const MIN_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

/// The acquire's delay sequence: starts at 2 s, doubles, caps at 30 s.
const ACQUIRE_BASE_DELAY_SECS: u64 = 2;
const ACQUIRE_CAP_DELAY_SECS: u64 = 30;

/// The deadline for one acquire. `None` means no budget: one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryDeadline {
    at: Option<Instant>,
    base_delay_secs: u64,
    cap_delay_secs: u64,
}

impl RetryDeadline {
    /// No retry budget — exactly one attempt, today's behaviour.
    pub fn none() -> Self {
        Self {
            at: None,
            base_delay_secs: ACQUIRE_BASE_DELAY_SECS,
            cap_delay_secs: ACQUIRE_CAP_DELAY_SECS,
        }
    }

    /// One deadline `budget` from `now` with the acquire's delay sequence. A
    /// zero budget is [`RetryDeadline::none`].
    pub fn from_budget(budget: Duration, now: Instant) -> Self {
        Self::with_delays(budget, now, ACQUIRE_BASE_DELAY_SECS, ACQUIRE_CAP_DELAY_SECS)
    }

    /// A deadline with its own delay sequence (the retire loop starts at 30 s
    /// and caps at 5 min).
    pub(crate) fn with_delays(
        budget: Duration,
        now: Instant,
        base_delay_secs: u64,
        cap_delay_secs: u64,
    ) -> Self {
        Self {
            at: (!budget.is_zero()).then(|| now + budget),
            base_delay_secs,
            cap_delay_secs,
        }
    }

    /// Whether a budget is set at all.
    pub fn is_active(&self) -> bool {
        self.at.is_some()
    }

    /// Budget left at `now`; `None` when no budget is set.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        self.at.map(|at| at.saturating_duration_since(now))
    }

    /// The timeout one attempt may use at `now`: `min(10 s, remaining)`,
    /// floored at 1 s. `None` when no budget is set — the caller keeps its own
    /// bound, so a zero budget changes nothing.
    pub fn attempt_timeout(&self, now: Instant) -> Option<Duration> {
        self.remaining(now)
            .map(|r| r.min(PER_ATTEMPT_TIMEOUT).max(MIN_ATTEMPT_TIMEOUT))
    }

    /// The sleep before the next attempt after `failures` consecutive transient
    /// failures, or `None` when no further attempt fits before the deadline.
    ///
    /// The ceiling is [`crate::agent_pusher::backoff_delay_secs`]; the delay is
    /// that ceiling scaled by `jitter_unit` in `[0, 1]` (full jitter, so a burst
    /// of continuations does not retry in lockstep against a recovering coord).
    /// Pure over its arguments so its bounds are testable without a clock.
    pub fn next_delay(&self, failures: u32, now: Instant, jitter_unit: f64) -> Option<Duration> {
        let remaining = self.remaining(now)?;
        let ceiling = crate::agent_pusher::backoff_delay_secs(
            self.base_delay_secs,
            failures,
            self.cap_delay_secs,
        );
        let unit = if jitter_unit.is_finite() {
            jitter_unit.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let delay = Duration::from_secs_f64(ceiling as f64 * unit);
        (delay < remaining).then_some(delay)
    }
}

/// One attempt's result: final (success or a real answer), or a transient
/// coord failure that may be retried while the budget lasts.
#[derive(Debug)]
pub(crate) enum Attempt<T, E> {
    Done(Result<T, E>),
    Transient(String),
}

/// Run `op` until it answers [`Attempt::Done`] or the deadline leaves no room
/// for another attempt. `op` receives the attempt's timeout (`None` when no
/// budget is set). On exhaustion the last transient message is mapped by
/// `exhausted`, annotated with the attempt count when more than one was made,
/// so a zero budget yields exactly today's message.
pub(crate) async fn retry_transient<T, E, F, Fut>(
    what: &str,
    deadline: RetryDeadline,
    mut op: F,
    exhausted: impl FnOnce(String) -> E,
) -> Result<T, E>
where
    F: FnMut(Option<Duration>) -> Fut,
    Fut: Future<Output = Attempt<T, E>>,
{
    let mut failures: u32 = 0;
    loop {
        let timeout = deadline.attempt_timeout(Instant::now());
        match op(timeout).await {
            Attempt::Done(r) => return r,
            Attempt::Transient(msg) => {
                failures = failures.saturating_add(1);
                let now = Instant::now();
                let jitter: f64 = rand::random();
                match deadline.next_delay(failures, now, jitter) {
                    Some(delay) => {
                        info!(
                            attempt = failures,
                            remaining_ms = deadline
                                .remaining(now)
                                .map(|r| r.as_millis() as u64)
                                .unwrap_or(0),
                            delay_ms = delay.as_millis() as u64,
                            "{what}: transient coord failure, retrying: {msg}"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    None => {
                        let msg = if failures > 1 {
                            format!("{msg} (after {failures} attempts)")
                        } else {
                            msg
                        };
                        return Err(exhausted(msg));
                    }
                }
            }
        }
    }
}

/// Whether an HTTP status from coord is worth retrying: a 5xx only.
pub(crate) fn status_is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
}

/// The whole `source()` chain of an error, outermost first, with links that
/// merely repeat their parent's text dropped. `{e:#}` on a bare
/// `reqwest::Error` prints the outer message only, which is why a log of
/// "error sending request" could not say whether a failure was a connect, a
/// DNS lookup or a timeout.
pub(crate) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        let m = c.to_string();
        if !m.is_empty() && !out.contains(&m) {
            out.push_str(": ");
            out.push_str(&m);
        }
        cur = c.source();
    }
    out
}

/// [`error_chain`] plus reqwest's own classification of the failure.
pub(crate) fn describe_reqwest_error(e: &reqwest::Error) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    if e.is_timeout() {
        kinds.push("timeout");
    }
    if e.is_connect() {
        kinds.push("connect");
    }
    if e.is_request() {
        kinds.push("request");
    }
    if e.is_body() {
        kinds.push("body");
    }
    if e.is_decode() {
        kinds.push("decode");
    }
    let chain = error_chain(e);
    if kinds.is_empty() {
        chain
    } else {
        format!("{chain} [{}]", kinds.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_budget_is_no_deadline_and_no_retry() {
        let now = Instant::now();
        let d = RetryDeadline::from_budget(Duration::ZERO, now);
        assert_eq!(d, RetryDeadline::none());
        assert!(!d.is_active());
        assert_eq!(d.attempt_timeout(now), None);
        assert_eq!(d.next_delay(1, now, 0.0), None);
    }

    #[test]
    fn delays_stay_within_the_doubling_cap() {
        let now = Instant::now();
        let d = RetryDeadline::from_budget(Duration::from_secs(10_000), now);
        let ceilings = [2u64, 4, 8, 16, 30, 30, 30];
        for (i, ceiling) in ceilings.iter().enumerate() {
            let failures = (i + 1) as u32;
            assert_eq!(
                d.next_delay(failures, now, 1.0),
                Some(Duration::from_secs(*ceiling)),
                "full-jitter ceiling at failure {failures}"
            );
            assert_eq!(d.next_delay(failures, now, 0.0), Some(Duration::ZERO));
            let mid = d.next_delay(failures, now, 0.5).unwrap();
            assert!(mid <= Duration::from_secs(*ceiling));
        }
        // An out-of-range jitter is clamped, never a longer sleep.
        assert_eq!(d.next_delay(1, now, 7.0), Some(Duration::from_secs(2)));
        assert_eq!(d.next_delay(1, now, f64::NAN), Some(Duration::from_secs(2)));
    }

    #[test]
    fn the_deadline_holds_across_attempts() {
        let start = Instant::now();
        let d = RetryDeadline::from_budget(Duration::from_secs(120), start);
        // Walk the worst case (every delay at its ceiling) and confirm no
        // planned sleep ends past the deadline.
        let mut now = start;
        let mut failures = 0;
        while let Some(delay) = d.next_delay(failures + 1, now, 1.0) {
            failures += 1;
            now += delay;
            assert!(now < start + Duration::from_secs(120));
        }
        assert!(failures > 1, "a 120 s budget allows repeated attempts");
        // Past the deadline nothing more is planned.
        let late = start + Duration::from_secs(121);
        assert_eq!(d.next_delay(1, late, 0.0), None);
    }

    #[test]
    fn attempt_timeout_is_min_of_ten_seconds_and_remaining() {
        let start = Instant::now();
        let d = RetryDeadline::from_budget(Duration::from_secs(120), start);
        assert_eq!(d.attempt_timeout(start), Some(PER_ATTEMPT_TIMEOUT));
        let near = start + Duration::from_secs(116);
        assert_eq!(d.attempt_timeout(near), Some(Duration::from_secs(4)));
        let spent = start + Duration::from_secs(130);
        assert_eq!(d.attempt_timeout(spent), Some(MIN_ATTEMPT_TIMEOUT));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_budget_makes_one_attempt_with_todays_message() {
        let mut calls = 0;
        let r: Result<(), String> = retry_transient(
            "test",
            RetryDeadline::none(),
            |timeout| {
                calls += 1;
                assert_eq!(timeout, None);
                async { Attempt::Transient("POST x: boom".to_string()) }
            },
            |m| m,
        )
        .await;
        assert_eq!(calls, 1);
        assert_eq!(r.unwrap_err(), "POST x: boom");
    }

    #[tokio::test(start_paused = true)]
    async fn transient_then_success_retries_until_done() {
        let mut calls = 0;
        let r: Result<u32, String> = retry_transient(
            "test",
            RetryDeadline::from_budget(Duration::from_secs(120), Instant::now()),
            |_| {
                calls += 1;
                let n = calls;
                async move {
                    if n < 3 {
                        Attempt::Transient(format!("fail {n}"))
                    } else {
                        Attempt::Done(Ok(n))
                    }
                }
            },
            |m| m,
        )
        .await;
        assert_eq!(r.unwrap(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_final_answer_is_never_retried() {
        let mut calls = 0;
        let r: Result<(), String> = retry_transient(
            "test",
            RetryDeadline::from_budget(Duration::from_secs(120), Instant::now()),
            |_| {
                calls += 1;
                async { Attempt::Done(Err("held".to_string())) }
            },
            |m| m,
        )
        .await;
        assert_eq!(calls, 1);
        assert_eq!(r.unwrap_err(), "held");
    }

    #[tokio::test(start_paused = true)]
    async fn an_exhausted_budget_reports_the_attempt_count() {
        let start = Instant::now();
        let mut calls = 0u32;
        let r: Result<(), String> = retry_transient(
            "test",
            RetryDeadline::from_budget(Duration::from_secs(120), start),
            |_| {
                calls += 1;
                async { Attempt::Transient("down".to_string()) }
            },
            |m| m,
        )
        .await;
        let msg = r.unwrap_err();
        assert!(calls > 1);
        assert_eq!(msg, format!("down (after {calls} attempts)"));
        assert!(Instant::now() <= start + Duration::from_secs(120));
    }

    #[test]
    fn status_classification() {
        assert!(status_is_transient(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        ));
        assert!(status_is_transient(reqwest::StatusCode::BAD_GATEWAY));
        assert!(status_is_transient(
            reqwest::StatusCode::SERVICE_UNAVAILABLE
        ));
        assert!(!status_is_transient(reqwest::StatusCode::CONFLICT));
        assert!(!status_is_transient(reqwest::StatusCode::BAD_REQUEST));
        assert!(!status_is_transient(reqwest::StatusCode::NOT_FOUND));
        assert!(!status_is_transient(reqwest::StatusCode::OK));
    }

    #[derive(Debug)]
    struct Layer(&'static str, Option<Box<Layer>>);
    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1
                .as_deref()
                .map(|l| l as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn error_chain_walks_every_source_and_drops_repeats() {
        let e = Layer(
            "error sending request",
            Some(Box::new(Layer(
                "client error (Connect)",
                Some(Box::new(Layer(
                    "client error (Connect)",
                    Some(Box::new(Layer("connection refused (os error 111)", None))),
                ))),
            ))),
        );
        assert_eq!(
            error_chain(&e),
            "error sending request: client error (Connect): connection refused (os error 111)"
        );
        assert_eq!(error_chain(&Layer("alone", None)), "alone");
    }

    #[tokio::test]
    async fn a_refused_connect_is_described_with_its_cause_and_kind() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().unwrap().port()
        };
        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect_err("nothing listens on a released port");
        let described = describe_reqwest_error(&err);
        assert!(
            described.contains("[") && described.contains("connect"),
            "{described}"
        );
        assert!(
            described.len() > err.to_string().len(),
            "the chain adds the cause: {described}"
        );
    }
}
