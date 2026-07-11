//! Tenant agentic-memory **synthesis poller** — the runner half of plan
//! `2026-07-11-tenant-memory-v1-1-close-the-loop` (Phase 2).
//!
//! ## What it does
//!
//! The web backend groups related tenant memories into *synthesis jobs*: a set
//! of member episodes/facts that should be distilled into one durable "mental
//! model". The runner is the compute for that distillation. This poller:
//!
//! 1. **Claims** a small batch of pending jobs
//!    (`POST /api/v1/memory/synthesis-jobs/claim {"limit": 4}`).
//! 2. For each claimed job, feeds its `member_texts` to the **warm Claude
//!    provider** ([`crate::ai_provider::claude_api_warm`]) under a stable,
//!    cache-friendly system prompt ("Memory-synthesis v1") and gets back one
//!    mental-model paragraph.
//! 3. **Reports** the result back:
//!    - success → `POST .../{job_id}/result {"result_text": "<text>"}`
//!    - failure → `POST .../{job_id}/result {"failure": "<reason>"}`
//!
//!    The backend does the embed + `mental_model` insert + member supersede.
//!    The runner NEVER writes memory rows for synthesis — it only claims,
//!    synthesizes, and posts.
//!
//! ## Gates + posture (mirrors [`crate::memory::tenant_sync`])
//!
//! 1. **Consent gate (hard)** — `Settings.cloud_sync_enabled`. Closed ⇒ the
//!    tick idles with ZERO network calls (not even the claim).
//! 2. **Unpaired** — no device JWT ⇒ idle, no calls, warn once. Jobs are never
//!    failed for lack of auth.
//! 3. **No credentials (headless / temp runner)** — the warm provider returns
//!    a `Disabled` outcome when neither a keychain API key nor a Claude CLI
//!    OAuth token is available. In that case the poller STOPS the tick and
//!    leaves the remaining claimed jobs *un-resulted* — the backend reaper
//!    returns them to `pending` after its timeout. It does NOT post a failure
//!    (the job is fine; this runner just can't do the work right now).
//! 4. **Other synthesis errors** (LLM error, empty/oversize output) ⇒ a
//!    `failure` POST so the job doesn't wedge, then the loop continues.
//! 5. **Never panics / never wedges** — every network or provider error is
//!    caught, logged via `tracing`, and the loop continues after backoff.
//!
//! ## Cadence
//!
//! Slow idle poll (~10 min) when a claim comes back empty; immediate re-poll
//! while jobs keep arriving (a non-empty claim is processed and then the loop
//! claims again right away). Transient failures back off exponentially.

use std::sync::{Arc, Once};
use std::time::Duration;

use serde_json::{json, Value as JsonValue};
use tracing::{debug, info, warn};

use super::tenant_sync::{resolve_web_base, BearerProvider, ConsentGate};

/// Number of jobs to claim per request (server accepts 1..4).
const CLAIM_LIMIT: u32 = 4;

/// Model for synthesis calls. Haiku-class — cheap + fast for a
/// distill-N-into-one summarization. Mirrors the emitter / coordinator
/// callout default.
const SYNTHESIS_MODEL: &str = "claude-haiku-4-5-20251001";

/// Output token cap. ~2 KB of prose lands well under this; the cap is a
/// backstop against a runaway generation.
const MAX_SYNTHESIS_TOKENS: u32 = 700;

/// Hard ceiling on the synthesized text (bytes). The system prompt asks for
/// ≤ 2 KB; this is the safety ceiling above which we post a `failure` rather
/// than pushing a garbage/runaway payload at the backend.
const MAX_RESULT_BYTES: usize = 4096;

/// Versioned, stable system prompt for the synthesis call ("Memory-synthesis
/// v1"). Stable text so the warm provider's ephemeral prompt cache can key off
/// it across calls in a session.
pub const MEMORY_SYNTHESIS_SYSTEM_V1: &str = "\
You distill several related memory episodes or facts from one tenant's agentic \
work history into a SINGLE durable mental model.\n\
\n\
Rules:\n\
- Output ONE mental model as plain prose. No preamble, no headings, no bullet \
lists, no markdown, no meta-commentary — just the model text itself.\n\
- Capture ONLY the durable generalization the episodes share: the pattern, \
rule, or stable fact that will still be true next week.\n\
- Do NOT speculate. Do NOT invent specifics (names, numbers, IDs, dates, file \
paths) that are not present in the episodes. If the episodes disagree, describe \
the general tendency rather than fabricating a resolution.\n\
- Be concise: stay under 2 KB. Drop episode-specific noise, timestamps, and \
one-off details that will not generalize.";

/// Initial delay before the first poll — let the app finish booting.
const INITIAL_DELAY: Duration = Duration::from_secs(60);
/// Idle cadence when a claim comes back empty (or consent/auth is off).
const TICK_IDLE: Duration = Duration::from_secs(600);
/// Small yield between successive non-empty claims (keeps the loop from
/// spinning while still draining a backlog "immediately").
const TICK_BUSY: Duration = Duration::from_secs(1);
/// Initial retry backoff for transient claim/transport failures.
const BACKOFF_INITIAL: Duration = Duration::from_secs(30);
/// Backoff ceiling.
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Outcome of synthesizing one job's member texts. The `Synthesizer` seam
/// returns this so tests can drive every branch without a live LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SynthOutcome {
    /// A mental model was produced. Carries warm-provider cache telemetry.
    Ok {
        text: String,
        cache_read_tokens: u64,
        cache_creation_tokens: u64,
    },
    /// No warm credentials available (headless / temp runner without a key or
    /// OAuth token). The job is LEFT un-resulted — never failed.
    Disabled,
    /// Any other synthesis failure (LLM error, empty/oversize output). The job
    /// gets a `failure` POST so it doesn't wedge.
    Failed(String),
}

/// The synthesis function seam. Production wraps the warm Claude provider;
/// tests inject a deterministic closure. Called inside `spawn_blocking` since
/// the production impl uses the blocking warm client.
pub type Synthesizer = Arc<dyn Fn(&[String]) -> SynthOutcome + Send + Sync>;

/// One job as returned by the claim endpoint.
#[derive(Debug, Clone, serde::Deserialize)]
struct ClaimedJob {
    job_id: String,
    #[serde(default)]
    member_texts: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ClaimResponse {
    #[serde(default)]
    jobs: Vec<ClaimedJob>,
}

/// Result of one [`MemorySynthesisPoller::poll_once`]. Drives the loop cadence
/// and is surfaced for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// Consent gate closed — no calls made.
    ConsentOff,
    /// Unpaired (no device JWT) — no calls made.
    NoAuth,
    /// Claim returned no jobs.
    Idle,
    /// Warm provider had no credentials — remaining jobs left for the reaper.
    Disabled,
    /// Processed this many jobs (each got a `result` or `failure` POST).
    Processed(usize),
    /// Transient claim/transport failure — retry after backoff.
    Retry(String),
}

/// Consent-gated, credential-aware memory-synthesis poller.
pub struct MemorySynthesisPoller {
    gate: ConsentGate,
    bearer: BearerProvider,
    synthesizer: Synthesizer,
    warned_no_auth: Once,
}

impl std::fmt::Debug for MemorySynthesisPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemorySynthesisPoller").finish()
    }
}

impl MemorySynthesisPoller {
    /// Production constructor: consent from `Settings.cloud_sync_enabled`,
    /// bearer from the default device-JWT slot, synthesizer = warm Claude.
    pub fn new() -> Self {
        Self::with_probes(
            Box::new(crate::settings::get_cloud_sync_enabled),
            Box::new(crate::auth::device_bearer),
            Arc::new(warm_synthesize),
        )
    }

    /// Injectable constructor for tests.
    pub fn with_probes(
        gate: ConsentGate,
        bearer: BearerProvider,
        synthesizer: Synthesizer,
    ) -> Self {
        Self {
            gate,
            bearer,
            synthesizer,
            warned_no_auth: Once::new(),
        }
    }

    /// Claim one batch and process it. See [`PollOutcome`] for the branches.
    pub(crate) async fn poll_once(&self, client: &reqwest::Client, base: &str) -> PollOutcome {
        if !(self.gate)() {
            return PollOutcome::ConsentOff;
        }
        let Some(bearer) = (self.bearer)() else {
            self.warned_no_auth.call_once(|| {
                warn!(
                    "memory_synthesis: no device JWT available — synthesis poller idle until \
                     this runner is paired"
                );
            });
            return PollOutcome::NoAuth;
        };

        let base = base.trim_end_matches('/');
        let claim_url = format!("{base}/api/v1/memory/synthesis-jobs/claim");
        let resp = client
            .post(&claim_url)
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&json!({ "limit": CLAIM_LIMIT }))
            .send()
            .await;

        let jobs = match parse_claim(resp).await {
            Ok(j) => j,
            Err(reason) => return PollOutcome::Retry(reason),
        };
        if jobs.is_empty() {
            return PollOutcome::Idle;
        }

        let mut processed = 0usize;
        for job in jobs {
            let synth = Arc::clone(&self.synthesizer);
            let texts = job.member_texts.clone();
            let outcome = match tokio::task::spawn_blocking(move || synth(&texts)).await {
                Ok(o) => o,
                Err(e) => {
                    // Join failure (panic in the closure) — treat as transient:
                    // leave the job un-resulted for the reaper, keep going.
                    warn!(job_id = %job.job_id, error = %e, "memory_synthesis: synth task join failed");
                    continue;
                }
            };

            match outcome {
                SynthOutcome::Disabled => {
                    info!(
                        "memory_synthesis: warm provider disabled (no credentials) — leaving \
                         claimed job(s) un-resulted for the backend reaper"
                    );
                    return PollOutcome::Disabled;
                }
                SynthOutcome::Ok {
                    text,
                    cache_read_tokens,
                    cache_creation_tokens,
                } => {
                    info!(
                        job_id = %job.job_id,
                        result_bytes = text.len(),
                        cache_read_tokens,
                        cache_creation_tokens,
                        "memory_synthesis: synthesized mental model"
                    );
                    self.post_result(
                        client,
                        base,
                        &bearer,
                        &job.job_id,
                        ResultBody::Success(text),
                    )
                    .await;
                    processed += 1;
                }
                SynthOutcome::Failed(reason) => {
                    warn!(job_id = %job.job_id, %reason, "memory_synthesis: synthesis failed — posting failure");
                    self.post_result(
                        client,
                        base,
                        &bearer,
                        &job.job_id,
                        ResultBody::Failure(reason),
                    )
                    .await;
                    processed += 1;
                }
            }
        }

        PollOutcome::Processed(processed)
    }

    /// POST a job result (success or failure). Best-effort: a rejected/failed
    /// POST is logged and the job is left for the backend reaper — never a
    /// panic, never a wedge.
    async fn post_result(
        &self,
        client: &reqwest::Client,
        base: &str,
        bearer: &str,
        job_id: &str,
        body: ResultBody,
    ) {
        let url = format!("{base}/api/v1/memory/synthesis-jobs/{job_id}/result");
        let payload = match &body {
            ResultBody::Success(text) => json!({ "result_text": text }),
            ResultBody::Failure(reason) => json!({ "failure": reason }),
        };
        match client
            .post(&url)
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!(
                    job_id,
                    kind = body.kind(),
                    "memory_synthesis: result posted"
                );
            }
            Ok(resp) => {
                warn!(
                    job_id,
                    status = %resp.status(),
                    kind = body.kind(),
                    "memory_synthesis: result POST rejected — job left for reaper"
                );
            }
            Err(e) => {
                warn!(job_id, error = %e, kind = body.kind(), "memory_synthesis: result POST failed — job left for reaper");
            }
        }
    }
}

impl Default for MemorySynthesisPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Which result body we're posting (for logging only).
enum ResultBody {
    Success(String),
    Failure(String),
}

impl ResultBody {
    fn kind(&self) -> &'static str {
        match self {
            ResultBody::Success(_) => "result",
            ResultBody::Failure(_) => "failure",
        }
    }
}

/// Parse the claim response, mapping every failure class to a `Retry` reason.
async fn parse_claim(
    resp: Result<reqwest::Response, reqwest::Error>,
) -> Result<Vec<ClaimedJob>, String> {
    let resp = resp.map_err(|e| format!("claim transport: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        let detail: String = detail.chars().take(256).collect();
        return Err(format!("claim {status}: {detail}"));
    }
    let parsed: ClaimResponse = resp.json().await.map_err(|e| format!("claim parse: {e}"))?;
    Ok(parsed.jobs)
}

/// Build the per-job user message from the member texts.
fn build_synthesis_user_message(member_texts: &[String]) -> String {
    let mut msg = String::from(
        "Distill the following related memory episodes/facts into ONE durable mental \
         model, following the rules exactly:\n\n",
    );
    for (i, t) in member_texts.iter().enumerate() {
        msg.push_str(&format!("Episode {}:\n{}\n\n", i + 1, t.trim()));
    }
    msg.push_str("Mental model:");
    msg
}

/// Production synthesizer: run the member texts through the warm Claude
/// provider. Runs on a blocking thread (the warm client is `reqwest::blocking`).
fn warm_synthesize(member_texts: &[String]) -> SynthOutcome {
    use crate::ai_provider::claude_api_warm;

    if member_texts.is_empty() {
        return SynthOutcome::Failed("no member texts to synthesize".to_string());
    }

    let claude_api_settings = crate::settings::get_ai_settings().claude_api;
    let user_message = build_synthesis_user_message(member_texts);

    let response = claude_api_warm::run_claude_api_warm(
        MEMORY_SYNTHESIS_SYSTEM_V1,
        &user_message,
        &claude_api_settings,
        Some(SYNTHESIS_MODEL),
        None,      // no doctor handle
        Some(0.2), // low temperature — durable generalization, not creativity
        Some(MAX_SYNTHESIS_TOKENS),
    );

    // No credentials (keychain key absent AND no Claude CLI OAuth): leave the
    // job for the reaper rather than failing it.
    if claude_api_warm::is_no_credential_error(&response) {
        return SynthOutcome::Disabled;
    }

    if !response.success {
        return SynthOutcome::Failed(
            response
                .error
                .unwrap_or_else(|| "warm provider returned an unspecified error".to_string()),
        );
    }

    let text = response.output.trim().to_string();
    if text.is_empty() {
        return SynthOutcome::Failed("synthesis produced empty output".to_string());
    }
    if text.len() > MAX_RESULT_BYTES {
        return SynthOutcome::Failed(format!(
            "synthesis exceeded {MAX_RESULT_BYTES} bytes ({} bytes)",
            text.len()
        ));
    }

    SynthOutcome::Ok {
        text,
        cache_read_tokens: response.cache_read_tokens.unwrap_or(0),
        cache_creation_tokens: response.cache_creation_tokens.unwrap_or(0),
    }
}

/// Spawn the memory-synthesis poll loop as a Tauri background task. Mirrors
/// `memory::scheduler::start_memory_scheduler`'s spawn shape.
pub fn start_memory_synthesis_poller() -> tauri::async_runtime::JoinHandle<()> {
    let poller = Arc::new(MemorySynthesisPoller::new());
    tauri::async_runtime::spawn(run_poll_loop(poller))
}

async fn run_poll_loop(poller: Arc<MemorySynthesisPoller>) {
    tokio::time::sleep(INITIAL_DELAY).await;
    info!(
        "memory_synthesis: synthesis poller started (idle cadence {}s)",
        TICK_IDLE.as_secs()
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|e| {
            warn!(error = %e, "memory_synthesis: reqwest client build failed; using default");
            reqwest::Client::new()
        });

    let warned_no_base = Once::new();
    let mut backoff = BACKOFF_INITIAL;

    loop {
        let Some(base) = resolve_web_base() else {
            warned_no_base.call_once(|| {
                warn!(
                    "memory_synthesis: no web backend base resolvable (QONTINUI_WEB_BASE unset \
                     and no profile coord_url) — synthesis poller idle until configured"
                );
            });
            tokio::time::sleep(TICK_IDLE).await;
            continue;
        };

        match poller.poll_once(&client, &base).await {
            PollOutcome::Processed(n) if n > 0 => {
                // Jobs are flowing — drain the backlog by re-polling promptly.
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_BUSY).await;
            }
            PollOutcome::Processed(_)
            | PollOutcome::Idle
            | PollOutcome::ConsentOff
            | PollOutcome::NoAuth
            | PollOutcome::Disabled => {
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_IDLE).await;
            }
            PollOutcome::Retry(reason) => {
                debug!(%reason, "memory_synthesis: poll failed; backing off");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, BACKOFF_MAX);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{Path, State as AxumState},
        routing::post,
        Json, Router,
    };
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokMutex;

    #[derive(Default)]
    struct SynthRecorder {
        claim_calls: usize,
        /// (job_id, body) for each result POST.
        result_calls: Vec<(String, JsonValue)>,
        /// Jobs to serve on the (first) claim; taken so a re-poll sees empty.
        jobs: Vec<JsonValue>,
    }

    fn job(id: &str, texts: &[&str]) -> JsonValue {
        json!({
            "job_id": id,
            "member_texts": texts.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        })
    }

    async fn spawn_fake_synth_web(jobs: Vec<JsonValue>) -> (String, Arc<TokMutex<SynthRecorder>>) {
        let rec = Arc::new(TokMutex::new(SynthRecorder {
            jobs,
            ..SynthRecorder::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/api/v1/memory/synthesis-jobs/claim",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<SynthRecorder>>>,
                     Json(_body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        g.claim_calls += 1;
                        let jobs = std::mem::take(&mut g.jobs);
                        Json(json!({ "jobs": jobs }))
                    },
                ),
            )
            .route(
                "/api/v1/memory/synthesis-jobs/{job_id}/result",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<SynthRecorder>>>,
                     Path(job_id): Path<String>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        let status = if body.get("failure").is_some() {
                            "recorded"
                        } else {
                            "applied"
                        };
                        g.result_calls.push((job_id, body));
                        Json(json!({ "status": status }))
                    },
                ),
            )
            .with_state(rec.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), rec)
    }

    fn poller(
        gate_open: bool,
        bearer: Option<&'static str>,
        synth: Synthesizer,
    ) -> MemorySynthesisPoller {
        MemorySynthesisPoller::with_probes(
            Box::new(move || gate_open),
            Box::new(move || bearer.map(str::to_string)),
            synth,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn consent_gate_off_makes_no_http_calls() {
        let (base, rec) = spawn_fake_synth_web(vec![job("j1", &["a", "b"])]).await;
        let p = poller(
            false,
            Some("test.jwt"),
            Arc::new(|_| SynthOutcome::Disabled),
        );
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::ConsentOff);
        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 0, "consent off ⇒ no claim call");
        assert!(g.result_calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpaired_no_bearer_makes_no_http_calls() {
        let (base, rec) = spawn_fake_synth_web(vec![job("j1", &["a"])]).await;
        let p = poller(true, None, Arc::new(|_| SynthOutcome::Disabled));
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::NoAuth);
        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 0, "no device JWT ⇒ no claim call");
        assert!(g.result_calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claim_synthesize_result_round_trip() {
        let (base, rec) = spawn_fake_synth_web(vec![job("j1", &["ep one", "ep two"])]).await;
        let synth: Synthesizer = Arc::new(|texts: &[String]| SynthOutcome::Ok {
            text: format!("synth:{}", texts.join("|")),
            cache_read_tokens: 5,
            cache_creation_tokens: 2,
        });
        let p = poller(true, Some("test.jwt"), synth);
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Processed(1));

        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 1);
        assert_eq!(g.result_calls.len(), 1);
        assert_eq!(g.result_calls[0].0, "j1");
        assert_eq!(g.result_calls[0].1["result_text"], "synth:ep one|ep two");
        assert!(
            g.result_calls[0].1.get("failure").is_none(),
            "success path must not send a failure"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_leaves_jobs_unresulted() {
        let (base, rec) = spawn_fake_synth_web(vec![job("j1", &["a"])]).await;
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled));
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Disabled);

        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 1, "claim still happens");
        assert!(
            g.result_calls.is_empty(),
            "warm provider disabled ⇒ no result/failure POST; job left for the reaper"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn synthesis_failure_posts_failure() {
        let (base, rec) = spawn_fake_synth_web(vec![job("j1", &["a"])]).await;
        let p = poller(
            true,
            Some("test.jwt"),
            Arc::new(|_| SynthOutcome::Failed("boom".to_string())),
        );
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Processed(1));

        let g = rec.lock().await;
        assert_eq!(g.result_calls.len(), 1);
        assert_eq!(g.result_calls[0].1["failure"], "boom");
        assert!(g.result_calls[0].1.get("result_text").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_claim_is_idle() {
        let (base, rec) = spawn_fake_synth_web(vec![]).await;
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled));
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Idle);
        assert!(rec.lock().await.result_calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claim_transport_error_is_retry() {
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled));
        // Unroutable — connection refused.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let outcome = p.poll_once(&client, "http://127.0.0.1:1").await;
        assert!(matches!(outcome, PollOutcome::Retry(_)), "got {outcome:?}");
    }

    #[test]
    fn user_message_includes_each_episode() {
        let msg = build_synthesis_user_message(&["alpha".to_string(), "beta".to_string()]);
        assert!(msg.contains("Episode 1:"));
        assert!(msg.contains("alpha"));
        assert!(msg.contains("Episode 2:"));
        assert!(msg.contains("beta"));
        assert!(msg.trim_end().ends_with("Mental model:"));
    }
}
