//! Tenant agentic-memory **job poller** — the runner half of plan
//! `2026-07-11-tenant-memory-v1-1-close-the-loop` (Phase 2, synthesis) and
//! `2026-07-13-runner-paid-embedding` (Phase 3, kind-dispatch + embedding).
//!
//! ## What it does
//!
//! The web backend queues *memory jobs* the runner is the compute for. Each
//! job carries a [`JobKind`] that says what work it wants. This poller:
//!
//! 1. **Claims** a small batch of pending jobs, filtered to the kinds this
//!    runner can serve
//!    (`POST /api/v1/memory/jobs/claim {"limit": 4, "kinds": [...]}`).
//! 2. **Dispatches on `kind`**:
//!    - [`JobKind::Synthesis`] — feeds `input_texts` to the **warm Claude
//!      provider** ([`crate::ai_provider::claude_api_warm`]) under a stable,
//!      cache-friendly system prompt ("Memory-synthesis v1"), gets back one
//!      mental-model paragraph, then embeds that paragraph locally.
//!    - [`JobKind::Embedding`] — batches `input_texts` through the local
//!      embedding service; no LLM is involved.
//! 3. **Reports** the result back to
//!    `POST /api/v1/memory/jobs/{job_id}/result`:
//!    - synthesis → `{"result": {"result_text", "embedding", "embedding_model"}}`
//!    - embedding → `{"result": {"embeddings": [...], "embedding_model"}}`
//!      (one vector per `input_texts` entry, same order)
//!    - failure   → `{"failure": "<reason>"}`
//!
//!    Both kinds ship a VECTOR, which is what lets the backend drop its own
//!    embed call entirely. The runner NEVER writes memory rows — it only
//!    claims, computes, and posts; the backend owns the inserts and supersedes.
//!
//! ## Gates + posture (mirrors [`crate::memory::tenant_sync`])
//!
//! 1. **Consent gate (hard)** — `Settings.cloud_sync_enabled`. Closed ⇒ the
//!    tick idles with ZERO network calls (not even the claim). This governs
//!    EVERY kind: claiming an `embedding` job pulls tenant content onto this
//!    device exactly as a `synthesis` job does.
//! 2. **Unpaired** — no device JWT ⇒ idle, no calls, warn once. Jobs are never
//!    failed for lack of auth.
//! 3. **This runner can't do the work right now** ⇒ STOP the tick and leave
//!    the claimed jobs *un-resulted*; the backend reaper returns them to
//!    `pending` after its lease timeout, and another runner (or this one,
//!    later) picks them up. Do NOT post a `failure` — a failure burns the
//!    job's `attempt` and eventually wedges a perfectly good job. Two
//!    conditions share this branch, and they are the same condition:
//!    - **No LLM credentials** (headless / temp runner: no keychain API key
//!      and no Claude CLI OAuth token) — the warm provider returns `Disabled`.
//!    - **Local embedding service unavailable** — the embed call errors.
//!    - **Local embedding service off-spec** — it ANSWERS, but with a vector
//!      that is not [`EMBEDDING_DIM`] wide, so it is not in the space
//!      [`EMBEDDING_MODEL_TAG`] names. Posting it would tag a foreign space as
//!      ours and earn a backend 422 that the best-effort result POST swallows,
//!      spinning claim → embed → 422 → reap forever. An embedder answering
//!      wrong is an embedder not working, so it shares this branch — under its
//!      own reason string, because unlike an outage it will not fix itself.
//!
//!      [`EMBEDDING_DIM`]: crate::database::embeddings::EMBEDDING_DIM
//! 4. **Bad job** (empty input, LLM error, empty/oversize output, unservable
//!    kind) ⇒ a `failure` POST so the job doesn't wedge, then the loop
//!    continues. The distinction from (3) is the whole failure taxonomy: (3)
//!    is about THIS RUNNER, (4) is about THE JOB.
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
use crate::database::embedding_client::{EmbedFailure, EmbeddingClient, EMBEDDING_MODEL_TAG};

/// Number of jobs to claim per request (server accepts 1..4).
const CLAIM_LIMIT: u32 = 4;

/// Job kinds this runner can serve, sent as the claim's `kinds` filter so the
/// server never hands us work we'd only have to fail.
const CLAIM_KINDS: [&str; 2] = ["synthesis", "embedding"];

/// Reason string for the "no LLM credentials" arm of [`PollOutcome::Disabled`].
const DISABLED_NO_LLM: &str = "warm provider has no credentials";
/// Reason string for the "local embedder down" arm of [`PollOutcome::Disabled`].
const DISABLED_NO_EMBEDDER: &str = "local embedding service unavailable";
/// Reason string for the "local embedder is UP and wrong" arm of
/// [`PollOutcome::Disabled`] — it answered 200 with a vector that is not in
/// this machine's embedding space.
///
/// Kept apart from [`DISABLED_NO_EMBEDDER`] because the operator action
/// differs: an outage resolves itself when the service comes back, an off-spec
/// vector is a misconfiguration that will keep answering wrong until someone
/// fixes it. Both leave the job for the reaper; only one of them is worth
/// waiting on.
const DISABLED_EMBEDDER_OFF_SPEC: &str = "local embedding service returned an off-spec vector";

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

/// What work a claimed job wants. The server is the authority on the wire
/// values; [`JobKind::Unknown`] catches a kind this runner build predates so a
/// newer server can never make an older runner panic (it gets a `failure`
/// instead — see [`PollOutcome`] rule 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// Distill `input_texts` into one mental model, and embed the result.
    Synthesis,
    /// Embed each of `input_texts`. No LLM involved.
    Embedding,
    #[serde(other)]
    Unknown,
}

/// One job as returned by the claim endpoint.
#[derive(Debug, Clone, serde::Deserialize)]
struct ClaimedJob {
    job_id: String,
    kind: JobKind,
    /// The texts to operate on. For `synthesis` these are the member
    /// episodes/facts; for `embedding` they are the texts to vectorize.
    #[serde(default)]
    input_texts: Vec<String>,
    /// The memory rows this job's result applies to. The runner does not write
    /// them (the backend owns the inserts) — they are carried for provenance
    /// and logging, so a job's blast radius is visible in the runner's logs.
    #[serde(default)]
    target_ids: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ClaimResponse {
    #[serde(default)]
    jobs: Vec<ClaimedJob>,
}

/// Result of one [`MemoryJobPoller::poll_once`]. Drives the loop cadence
/// and is surfaced for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    /// Consent gate closed — no calls made.
    ConsentOff,
    /// Unpaired (no device JWT) — no calls made.
    NoAuth,
    /// Claim returned no jobs.
    Idle,
    /// This runner can't do the work right now (no LLM credentials, or the
    /// local embedding service is down). The tick STOPS and the remaining
    /// claimed jobs are left un-resulted for the backend reaper — never
    /// failed. Carries the reason for logs/tests.
    Disabled(&'static str),
    /// Processed this many jobs (each got a `result` or `failure` POST).
    Processed(usize),
    /// Transient claim/transport failure — retry after backoff.
    Retry(String),
}

/// Consent-gated, credential-aware memory-job poller.
pub struct MemoryJobPoller {
    gate: ConsentGate,
    bearer: BearerProvider,
    synthesizer: Synthesizer,
    /// Local embedding service client — both job kinds route through it.
    embedder: EmbeddingClient,
    warned_no_auth: Once,
}

impl std::fmt::Debug for MemoryJobPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryJobPoller").finish()
    }
}

impl MemoryJobPoller {
    /// Production constructor: consent from `Settings.cloud_sync_enabled`,
    /// bearer from the default device-JWT slot, synthesizer = warm Claude,
    /// embedder = the local embedding service.
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
            embedder: EmbeddingClient::new(),
            warned_no_auth: Once::new(),
        }
    }

    /// Point the poller at a specific embedding service (tests aim this at a
    /// local fake, or at an unroutable host to drive the embedder-down path).
    pub fn with_embedder(mut self, embedder: EmbeddingClient) -> Self {
        self.embedder = embedder;
        self
    }

    /// Claim one batch and process it. See [`PollOutcome`] for the branches.
    pub(crate) async fn poll_once(&self, client: &reqwest::Client, base: &str) -> PollOutcome {
        if !(self.gate)() {
            return PollOutcome::ConsentOff;
        }
        let Some(bearer) = (self.bearer)() else {
            self.warned_no_auth.call_once(|| {
                warn!(
                    "memory_jobs: no device JWT available — job poller idle until \
                     this runner is paired"
                );
            });
            return PollOutcome::NoAuth;
        };

        let base = base.trim_end_matches('/');
        let claim_url = format!("{base}/api/v1/memory/jobs/claim");
        // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/memory/jobs/claim`.
        // It carries the device JWT, but the peer is the web backend, not coord.
        let resp = client
            .post(&claim_url)
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&json!({ "limit": CLAIM_LIMIT, "kinds": CLAIM_KINDS }))
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
            // Dispatch on the job's kind. Each arm yields either a result body
            // to post, or a Disabled reason that stops the tick and leaves this
            // job (and the rest of the batch) for the backend reaper.
            let body = match job.kind {
                JobKind::Synthesis => match self.run_synthesis_job(&job).await {
                    Ok(body) => body,
                    Err(JobHalt::Disabled(reason)) => {
                        info!(
                            job_id = %job.job_id,
                            kind = "synthesis",
                            %reason,
                            "memory_jobs: this runner can't do the work right now — leaving \
                             claimed job(s) un-resulted for the backend reaper"
                        );
                        return PollOutcome::Disabled(reason);
                    }
                    Err(JobHalt::Skip) => continue,
                },
                JobKind::Embedding => match self.run_embedding_job(&job).await {
                    Ok(body) => body,
                    Err(JobHalt::Disabled(reason)) => {
                        info!(
                            job_id = %job.job_id,
                            kind = "embedding",
                            %reason,
                            "memory_jobs: this runner can't do the work right now — leaving \
                             claimed job(s) un-resulted for the backend reaper"
                        );
                        return PollOutcome::Disabled(reason);
                    }
                    Err(JobHalt::Skip) => continue,
                },
                JobKind::Unknown => {
                    // We asked for kinds we can serve, so this is a server
                    // contract break, not a capability gap. Fail it: leaving it
                    // un-resulted would have the reaper hand it straight back.
                    warn!(
                        job_id = %job.job_id,
                        "memory_jobs: claimed a job of an unknown kind — posting failure"
                    );
                    ResultBody::Failure(
                        "runner cannot serve this job kind (unknown to this build)".to_string(),
                    )
                }
            };

            self.post_result(client, base, &bearer, &job.job_id, body)
                .await;
            processed += 1;
        }

        PollOutcome::Processed(processed)
    }

    /// `kind="synthesis"`: distill the member texts via the warm Claude
    /// provider, then embed the synthesized text locally. Posting the vector
    /// alongside the text is what lets the backend drop its own embed call.
    async fn run_synthesis_job(&self, job: &ClaimedJob) -> Result<ResultBody, JobHalt> {
        let synth = Arc::clone(&self.synthesizer);
        let texts = job.input_texts.clone();
        let outcome = match tokio::task::spawn_blocking(move || synth(&texts)).await {
            Ok(o) => o,
            Err(e) => {
                // Join failure (panic in the closure) — treat as transient:
                // leave the job un-resulted for the reaper, keep going.
                warn!(job_id = %job.job_id, error = %e, "memory_jobs: synth task join failed");
                return Err(JobHalt::Skip);
            }
        };

        let (text, cache_read_tokens, cache_creation_tokens) = match outcome {
            SynthOutcome::Disabled => return Err(JobHalt::Disabled(DISABLED_NO_LLM)),
            SynthOutcome::Failed(reason) => {
                warn!(job_id = %job.job_id, %reason, "memory_jobs: synthesis failed — posting failure");
                return Ok(ResultBody::Failure(reason));
            }
            SynthOutcome::Ok {
                text,
                cache_read_tokens,
                cache_creation_tokens,
            } => (text, cache_read_tokens, cache_creation_tokens),
        };

        let embedding = self.embed_one(&job.job_id, &text).await?;

        info!(
            job_id = %job.job_id,
            targets = job.target_ids.len(),
            result_bytes = text.len(),
            cache_read_tokens,
            cache_creation_tokens,
            "memory_jobs: synthesized mental model + embedded it locally"
        );
        Ok(ResultBody::Synthesis { text, embedding })
    }

    /// `kind="embedding"`: batch the job's texts through the local embedding
    /// service. One vector per input, same order — the backend zips them back
    /// onto `target_ids` positionally.
    async fn run_embedding_job(&self, job: &ClaimedJob) -> Result<ResultBody, JobHalt> {
        if job.input_texts.is_empty() {
            // A bad job, not a runner outage: fail it rather than letting the
            // reaper hand back an empty job forever.
            warn!(job_id = %job.job_id, "memory_jobs: embedding job has no input texts — posting failure");
            return Ok(ResultBody::Failure(
                "embedding job carried no input texts".to_string(),
            ));
        }

        // `_checked`: the count check below already treats a broken embedder
        // contract as "leave it for the reaper", and a vector of the wrong
        // WIDTH is the same class of break — one the backend, not this runner,
        // would otherwise be the first to notice (with a 422 `post_result`
        // swallows).
        let embeddings = match self
            .embedder
            .compute_batch_embeddings_checked(&job.input_texts)
            .await
        {
            Ok(e) => e,
            Err(EmbedFailure::WrongWidth { got }) => {
                warn!(
                    job_id = %job.job_id,
                    got,
                    want = crate::database::embeddings::EMBEDDING_DIM,
                    "memory_jobs: local embedder returned an off-spec vector width"
                );
                return Err(JobHalt::Disabled(DISABLED_EMBEDDER_OFF_SPEC));
            }
            Err(e) => {
                debug!(job_id = %job.job_id, error = %e, "memory_jobs: local embed failed");
                return Err(JobHalt::Disabled(DISABLED_NO_EMBEDDER));
            }
        };
        if embeddings.len() != job.input_texts.len() {
            // The embedding service broke its contract. That's this runner
            // misbehaving, not a bad job — leave the job for the reaper.
            warn!(
                job_id = %job.job_id,
                got = embeddings.len(),
                want = job.input_texts.len(),
                "memory_jobs: embedding service returned the wrong vector count"
            );
            return Err(JobHalt::Disabled(DISABLED_NO_EMBEDDER));
        }

        info!(
            job_id = %job.job_id,
            targets = job.target_ids.len(),
            vectors = embeddings.len(),
            "memory_jobs: embedded job inputs locally"
        );
        Ok(ResultBody::Embedding(embeddings))
    }

    /// Embed one text, mapping a local-embedder outage onto the leave-it-for-
    /// the-reaper branch rather than a `failure`.
    ///
    /// `_checked` because this vector LEAVES the machine, tagged with
    /// [`EMBEDDING_MODEL_TAG`]: the backend rejects a non-`EMBEDDING_DIM`
    /// vector with a 422, and `post_result` swallows a rejected POST
    /// best-effort — so an off-spec embedder would spin claim → embed → 422 →
    /// reap forever, with a debug line as the only trace. An off-spec vector is
    /// the embedder not working, so it takes the same branch as one that is
    /// down.
    async fn embed_one(&self, job_id: &str, text: &str) -> Result<Vec<f32>, JobHalt> {
        match self.embedder.compute_text_embedding_checked(text).await {
            Ok(e) => Ok(e),
            Err(EmbedFailure::WrongWidth { got }) => {
                warn!(
                    job_id,
                    got,
                    want = crate::database::embeddings::EMBEDDING_DIM,
                    "memory_jobs: local embedder returned an off-spec vector width"
                );
                Err(JobHalt::Disabled(DISABLED_EMBEDDER_OFF_SPEC))
            }
            Err(e) => {
                debug!(job_id, error = %e, "memory_jobs: local embed failed");
                Err(JobHalt::Disabled(DISABLED_NO_EMBEDDER))
            }
        }
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
        let url = format!("{base}/api/v1/memory/jobs/{job_id}/result");
        let payload = match &body {
            ResultBody::Synthesis { text, embedding } => json!({
                "result": {
                    "result_text": text,
                    "embedding": embedding,
                    "embedding_model": EMBEDDING_MODEL_TAG,
                }
            }),
            ResultBody::Embedding(embeddings) => json!({
                "result": {
                    "embeddings": embeddings,
                    "embedding_model": EMBEDDING_MODEL_TAG,
                }
            }),
            ResultBody::Failure(reason) => json!({ "failure": reason }),
        };
        // coord-auth-exempt(not-coord): `qontinui-web` `/api/v1/memory/jobs/*`
        // result publish.
        match client
            .post(&url)
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                debug!(job_id, kind = body.kind(), "memory_jobs: result posted");
            }
            Ok(resp) => {
                warn!(
                    job_id,
                    status = %resp.status(),
                    kind = body.kind(),
                    "memory_jobs: result POST rejected — job left for reaper"
                );
            }
            Err(e) => {
                warn!(job_id, error = %e, kind = body.kind(), "memory_jobs: result POST failed — job left for reaper");
            }
        }
    }
}

impl Default for MemoryJobPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// What we're posting back for a job.
enum ResultBody {
    /// A synthesized mental model plus its locally-computed vector.
    Synthesis { text: String, embedding: Vec<f32> },
    /// One vector per the job's `input_texts`, in order.
    Embedding(Vec<Vec<f32>>),
    /// The job itself is bad — burn an `attempt` so it can't wedge.
    Failure(String),
}

impl ResultBody {
    fn kind(&self) -> &'static str {
        match self {
            ResultBody::Synthesis { .. } => "synthesis_result",
            ResultBody::Embedding(_) => "embedding_result",
            ResultBody::Failure(_) => "failure",
        }
    }
}

/// Why an arm produced no result body. Distinct from [`ResultBody::Failure`],
/// which IS a result (a negative one) — these two never post anything.
enum JobHalt {
    /// This runner can't do the work right now. Stop the tick; leave the
    /// claimed jobs un-resulted for the backend reaper. Never a `failure` POST
    /// — that would burn the job's `attempt` for a condition the job did not
    /// cause.
    Disabled(&'static str),
    /// Skip just this job (e.g. a panicking synthesizer closure) and continue
    /// the batch. Also leaves the job for the reaper.
    Skip,
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

/// Spawn the memory-job poll loop as a Tauri background task. Mirrors
/// `memory::scheduler::start_memory_scheduler`'s spawn shape.
pub fn start_memory_job_poller() -> tauri::async_runtime::JoinHandle<()> {
    let poller = Arc::new(MemoryJobPoller::new());
    tauri::async_runtime::spawn(run_poll_loop(poller))
}

async fn run_poll_loop(poller: Arc<MemoryJobPoller>) {
    tokio::time::sleep(INITIAL_DELAY).await;
    info!(
        kinds = ?CLAIM_KINDS,
        "memory_jobs: job poller started (idle cadence {}s)",
        TICK_IDLE.as_secs()
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|e| {
            warn!(error = %e, "memory_jobs: reqwest client build failed; using default");
            reqwest::Client::new()
        });

    let warned_no_base = Once::new();
    let mut backoff = BACKOFF_INITIAL;

    loop {
        let Some(base) = resolve_web_base() else {
            warned_no_base.call_once(|| {
                warn!(
                    "memory_jobs: no web backend base resolvable (QONTINUI_WEB_BASE unset \
                     and no profile coord_url) — job poller idle until configured"
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
            | PollOutcome::Disabled(_) => {
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_IDLE).await;
            }
            PollOutcome::Retry(reason) => {
                debug!(%reason, "memory_jobs: poll failed; backing off");
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
    struct JobRecorder {
        claim_calls: usize,
        /// Bodies of each claim POST — proves the `kinds` filter goes out.
        claim_bodies: Vec<JsonValue>,
        /// (job_id, body) for each result POST.
        result_calls: Vec<(String, JsonValue)>,
        /// Jobs to serve on the (first) claim; taken so a re-poll sees empty.
        jobs: Vec<JsonValue>,
    }

    fn synth_job(id: &str, texts: &[&str]) -> JsonValue {
        json!({
            "job_id": id,
            "kind": "synthesis",
            "input_texts": texts.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
            "target_ids": ["11111111-1111-4111-8111-111111111111"],
        })
    }

    fn embed_job(id: &str, texts: &[&str]) -> JsonValue {
        json!({
            "job_id": id,
            "kind": "embedding",
            "input_texts": texts.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
            "target_ids": texts
                .iter()
                .enumerate()
                .map(|(i, _)| format!("2222222{i}-2222-4222-8222-222222222222"))
                .collect::<Vec<_>>(),
        })
    }

    async fn spawn_fake_jobs_web(jobs: Vec<JsonValue>) -> (String, Arc<TokMutex<JobRecorder>>) {
        let rec = Arc::new(TokMutex::new(JobRecorder {
            jobs,
            ..JobRecorder::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/api/v1/memory/jobs/claim",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<JobRecorder>>>,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        g.claim_calls += 1;
                        g.claim_bodies.push(body);
                        let jobs = std::mem::take(&mut g.jobs);
                        Json(json!({ "jobs": jobs }))
                    },
                ),
            )
            .route(
                "/api/v1/memory/jobs/{job_id}/result",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<JobRecorder>>>,
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

    /// Deterministic stand-in for the local embedding service. Serves both
    /// routes `EmbeddingClient` uses (`compute-text`, and the `compute-batch`
    /// URL it derives from it) and returns index-keyed 384-dim vectors so a
    /// test can prove per-input alignment.
    async fn spawn_fake_embedder() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/api/embeddings/compute-batch",
                post(|Json(body): Json<JsonValue>| async move {
                    let n = body["texts"].as_array().map(Vec::len).unwrap_or(0);
                    let embeddings: Vec<Vec<f32>> = (0..n).map(|i| vec![i as f32; 384]).collect();
                    Json(json!({ "embeddings": embeddings }))
                }),
            )
            .route(
                "/api/embeddings/compute-text",
                post(|| async { Json(json!({ "embedding": vec![0.5f32; 384] })) }),
            );
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/api/embeddings/compute-text")
    }

    /// A poller whose embedder is up (points at a local fake).
    async fn poller(
        gate_open: bool,
        bearer: Option<&'static str>,
        synth: Synthesizer,
    ) -> MemoryJobPoller {
        let embed_url = spawn_fake_embedder().await;
        poller_with_embed_url(gate_open, bearer, synth, &embed_url)
    }

    fn poller_with_embed_url(
        gate_open: bool,
        bearer: Option<&'static str>,
        synth: Synthesizer,
        embed_url: &str,
    ) -> MemoryJobPoller {
        MemoryJobPoller::with_probes(
            Box::new(move || gate_open),
            Box::new(move || bearer.map(str::to_string)),
            synth,
        )
        .with_embedder(EmbeddingClient::with_url(embed_url))
    }

    /// Nothing listens on port 1 — connection refused for both the batch route
    /// and `compute_batch_embeddings`' per-text fallback.
    const EMBEDDER_DOWN_URL: &str = "http://127.0.0.1:1/api/embeddings/compute-text";

    fn ok_synth() -> Synthesizer {
        Arc::new(|texts: &[String]| SynthOutcome::Ok {
            text: format!("synth:{}", texts.join("|")),
            cache_read_tokens: 5,
            cache_creation_tokens: 2,
        })
    }

    // -----------------------------------------------------------------------
    // Gates — these govern EVERY kind, not just synthesis
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn consent_gate_off_makes_no_http_calls() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a", "b"])]).await;
        let p = poller(
            false,
            Some("test.jwt"),
            Arc::new(|_| SynthOutcome::Disabled),
        )
        .await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::ConsentOff);
        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 0, "consent off ⇒ no claim call");
        assert!(g.result_calls.is_empty());
    }

    /// Claiming an `embedding` job pulls tenant content onto this device just
    /// as a `synthesis` job does, so the SAME consent gate must govern it.
    #[tokio::test(flavor = "multi_thread")]
    async fn consent_gate_off_blocks_embedding_jobs_too() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &["a"])]).await;
        let p = poller(false, Some("test.jwt"), ok_synth()).await;
        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::ConsentOff
        );
        assert_eq!(
            rec.lock().await.claim_calls,
            0,
            "consent off ⇒ not even a claim for embedding work"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpaired_no_bearer_makes_no_http_calls() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a"])]).await;
        let p = poller(true, None, Arc::new(|_| SynthOutcome::Disabled)).await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::NoAuth);
        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 0, "no device JWT ⇒ no claim call");
        assert!(g.result_calls.is_empty());
    }

    // -----------------------------------------------------------------------
    // Claim contract
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn claim_filters_to_the_kinds_this_runner_serves() {
        let (base, rec) = spawn_fake_jobs_web(vec![]).await;
        let p = poller(true, Some("test.jwt"), ok_synth()).await;
        p.poll_once(&reqwest::Client::new(), &base).await;

        let g = rec.lock().await;
        assert_eq!(g.claim_bodies.len(), 1);
        assert_eq!(g.claim_bodies[0]["limit"], CLAIM_LIMIT);
        assert_eq!(
            g.claim_bodies[0]["kinds"],
            json!(["synthesis", "embedding"]),
            "the claim must name the kinds we can serve, so the server never \
             hands us work we'd only have to fail"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_claim_is_idle() {
        let (base, rec) = spawn_fake_jobs_web(vec![]).await;
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled)).await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Idle);
        assert!(rec.lock().await.result_calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn claim_transport_error_is_retry() {
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled)).await;
        // Unroutable — connection refused.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let outcome = p.poll_once(&client, "http://127.0.0.1:1").await;
        assert!(matches!(outcome, PollOutcome::Retry(_)), "got {outcome:?}");
    }

    // -----------------------------------------------------------------------
    // kind = "synthesis"
    // -----------------------------------------------------------------------

    /// Synthesis now posts the text AND its vector — that pair is what lets
    /// the backend drop its own embed call.
    #[tokio::test(flavor = "multi_thread")]
    async fn synthesis_job_posts_text_and_its_embedding() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["ep one", "ep two"])]).await;
        let p = poller(true, Some("test.jwt"), ok_synth()).await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Processed(1));

        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 1);
        assert_eq!(g.result_calls.len(), 1);
        assert_eq!(g.result_calls[0].0, "j1");
        let result = &g.result_calls[0].1["result"];
        assert_eq!(result["result_text"], "synth:ep one|ep two");
        assert_eq!(
            result["embedding"].as_array().map(Vec::len),
            Some(384),
            "the synthesized text must ship WITH its vector"
        );
        assert_eq!(result["embedding_model"], EMBEDDING_MODEL_TAG);
        assert!(
            g.result_calls[0].1.get("failure").is_none(),
            "success path must not send a failure"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn synthesis_failure_posts_failure() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a"])]).await;
        let p = poller(
            true,
            Some("test.jwt"),
            Arc::new(|_| SynthOutcome::Failed("boom".to_string())),
        )
        .await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Processed(1));

        let g = rec.lock().await;
        assert_eq!(g.result_calls.len(), 1);
        assert_eq!(g.result_calls[0].1["failure"], "boom");
        assert!(g.result_calls[0].1.get("result").is_none());
    }

    // -----------------------------------------------------------------------
    // kind = "embedding"
    // -----------------------------------------------------------------------

    /// One vector per `input_texts` entry, SAME ORDER — the backend zips them
    /// back onto `target_ids` positionally, so an order bug silently attaches
    /// every vector to the wrong memory. The fake embedder returns index-keyed
    /// vectors so that bug cannot pass.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedding_job_posts_one_vector_per_input_in_order() {
        let (base, rec) =
            spawn_fake_jobs_web(vec![embed_job("e1", &["first", "second", "third"])]).await;
        let p = poller(true, Some("test.jwt"), ok_synth()).await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Processed(1));

        let g = rec.lock().await;
        assert_eq!(g.result_calls.len(), 1);
        assert_eq!(g.result_calls[0].0, "e1");
        let result = &g.result_calls[0].1["result"];
        let embeddings = result["embeddings"].as_array().expect("embeddings array");
        assert_eq!(embeddings.len(), 3, "one vector per input text");
        for (i, e) in embeddings.iter().enumerate() {
            let v = e.as_array().unwrap();
            assert_eq!(v.len(), 384, "MiniLM-L6-v2 is 384-dimensional");
            assert_eq!(
                v[0].as_f64().unwrap(),
                i as f64,
                "vector {i} is out of order — the backend zips these onto \
                 target_ids positionally"
            );
        }
        assert_eq!(result["embedding_model"], EMBEDDING_MODEL_TAG);
        assert!(
            result.get("result_text").is_none(),
            "an embedding job has no text result"
        );
    }

    /// The embedding arm must not touch the LLM at all: a synthesizer that
    /// would panic if called proves the dispatch never routes there.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedding_job_never_calls_the_llm() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &["a"])]).await;
        let never: Synthesizer =
            Arc::new(|_| panic!("the embedding arm must never invoke the synthesizer"));
        let p = poller(true, Some("test.jwt"), never).await;
        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Processed(1)
        );
        assert!(rec.lock().await.result_calls[0].1["result"]["embeddings"].is_array());
    }

    /// An embedding job with no inputs is a BAD JOB (not a runner outage), so
    /// it gets a failure — leaving it un-resulted would have the reaper hand
    /// the same empty job back forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedding_job_with_no_inputs_posts_failure() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &[])]).await;
        let p = poller(true, Some("test.jwt"), ok_synth()).await;
        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Processed(1)
        );
        let g = rec.lock().await;
        assert!(g.result_calls[0].1["failure"]
            .as_str()
            .unwrap()
            .contains("no input texts"));
    }

    // -----------------------------------------------------------------------
    // "This runner can't do the work right now" — the shared branch
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn no_llm_credentials_leaves_jobs_unresulted() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a"])]).await;
        let p = poller(true, Some("test.jwt"), Arc::new(|_| SynthOutcome::Disabled)).await;
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Disabled(DISABLED_NO_LLM));

        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 1, "claim still happens");
        assert!(
            g.result_calls.is_empty(),
            "warm provider disabled ⇒ no result/failure POST; job left for the reaper"
        );
    }

    /// A local embedder that is down is "this runner can't do the work right
    /// now", NOT a bad job: it takes the same branch as missing LLM
    /// credentials. Posting a `failure` here would burn the job's `attempt`
    /// and eventually wedge a perfectly good job.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedder_down_leaves_embedding_job_unresulted() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &["a", "b"])]).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), EMBEDDER_DOWN_URL);
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Disabled(DISABLED_NO_EMBEDDER));

        let g = rec.lock().await;
        assert_eq!(g.claim_calls, 1, "claim still happens");
        assert!(
            g.result_calls.is_empty(),
            "embedder down ⇒ NO failure POST (that would burn `attempt`); the \
             job is left for the backend reaper to return to pending"
        );
    }

    /// Same posture on the synthesis path: the LLM succeeded, but we cannot
    /// produce the vector the backend now requires — so the job is left for
    /// the reaper rather than failed.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedder_down_leaves_synthesis_job_unresulted() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a"])]).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), EMBEDDER_DOWN_URL);
        let outcome = p.poll_once(&reqwest::Client::new(), &base).await;
        assert_eq!(outcome, PollOutcome::Disabled(DISABLED_NO_EMBEDDER));
        assert!(
            rec.lock().await.result_calls.is_empty(),
            "a synthesized text we cannot embed must not be posted, and must \
             not be failed either"
        );
    }

    /// The tick STOPS on the disabled branch — the rest of the claimed batch
    /// is left for the reaper too, rather than being burned one by one.
    #[tokio::test(flavor = "multi_thread")]
    async fn embedder_down_stops_the_tick_and_abandons_the_rest_of_the_batch() {
        let (base, rec) =
            spawn_fake_jobs_web(vec![embed_job("e1", &["a"]), embed_job("e2", &["b"])]).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), EMBEDDER_DOWN_URL);
        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Disabled(DISABLED_NO_EMBEDDER)
        );
        assert!(
            rec.lock().await.result_calls.is_empty(),
            "every claimed job in the batch is left un-resulted"
        );
    }

    /// A fake embedding service that is UP and answering with vectors of
    /// `dims` width, on both endpoints (`compute_batch_embeddings`
    /// derives the batch URL from the single one).
    async fn spawn_embedder_of_width(dims: usize) -> String {
        let app: Router = Router::new()
            .route(
                "/api/embeddings/compute-text",
                post(move || async move { Json(json!({ "embedding": vec![0.1f32; dims] })) }),
            )
            .route(
                "/api/embeddings/compute-batch",
                post(move |Json(body): Json<JsonValue>| async move {
                    let n = body
                        .get("texts")
                        .and_then(|t| t.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    Json(json!({ "embeddings": vec![vec![0.1f32; dims]; n] }))
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}/api/embeddings/compute-text")
    }

    /// An embedder that ANSWERS but in the wrong space is the embedder not
    /// working. Posting its vectors would tag a foreign space as
    /// `EMBEDDING_MODEL_TAG` and earn a backend 422 that `post_result`
    /// swallows — claim → embed → 422 → reap, forever. Leave it for the reaper
    /// under its own reason instead.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_off_spec_embedder_leaves_the_embedding_job_unresulted() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &["a", "b"])]).await;
        let url = spawn_embedder_of_width(crate::database::embeddings::EMBEDDING_DIM / 2).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), &url);

        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Disabled(DISABLED_EMBEDDER_OFF_SPEC),
            "an up-but-wrong embedder must be named as such, NOT as an outage — \
             the operator action differs"
        );
        assert!(
            rec.lock().await.result_calls.is_empty(),
            "an off-spec vector must never be POSTed, and the job must not be \
             failed either (that would burn `attempt`)"
        );
    }

    /// Same on the synthesis path, where the vector is computed after the LLM
    /// has already run.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_off_spec_embedder_leaves_the_synthesis_job_unresulted() {
        let (base, rec) = spawn_fake_jobs_web(vec![synth_job("j1", &["a"])]).await;
        let url = spawn_embedder_of_width(7).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), &url);

        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Disabled(DISABLED_EMBEDDER_OFF_SPEC)
        );
        assert!(
            rec.lock().await.result_calls.is_empty(),
            "a synthesized text we cannot embed IN OUR SPACE must not be posted"
        );
    }

    /// The positive control for both tests above: a right-width embedder
    /// posts its result as usual, so the guard is proven to reject the wrong
    /// width rather than everything.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_right_width_embedder_still_posts_its_result() {
        let (base, rec) = spawn_fake_jobs_web(vec![embed_job("e1", &["a"])]).await;
        let url = spawn_embedder_of_width(crate::database::embeddings::EMBEDDING_DIM).await;
        let p = poller_with_embed_url(true, Some("test.jwt"), ok_synth(), &url);

        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Processed(1)
        );
        let g = rec.lock().await;
        let (_, body) = g.result_calls.first().expect("the result must be posted");
        assert_eq!(body["result"]["embedding_model"], EMBEDDING_MODEL_TAG);
        assert_eq!(
            body["result"]["embeddings"][0].as_array().unwrap().len(),
            crate::database::embeddings::EMBEDDING_DIM
        );
    }

    // -----------------------------------------------------------------------
    // Kind parsing
    // -----------------------------------------------------------------------

    #[test]
    fn job_kind_parses_known_kinds_and_tolerates_new_ones() {
        let parse = |k: &str| -> JobKind {
            serde_json::from_value(json!({
                "job_id": "j", "kind": k, "input_texts": [], "target_ids": []
            }))
            .map(|j: ClaimedJob| j.kind)
            .unwrap()
        };
        assert_eq!(parse("synthesis"), JobKind::Synthesis);
        assert_eq!(parse("embedding"), JobKind::Embedding);
        // A kind from a newer server must not fail deserialization — that
        // would strand the WHOLE claim batch as a parse Retry.
        assert_eq!(parse("reranking"), JobKind::Unknown);
    }

    /// An unservable kind is failed, not left for the reaper: we asked for the
    /// kinds we serve, so getting another back is a server contract break, and
    /// the reaper would only hand it straight back.
    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_kind_posts_failure() {
        let (base, rec) = spawn_fake_jobs_web(vec![json!({
            "job_id": "x1", "kind": "reranking", "input_texts": ["a"], "target_ids": []
        })])
        .await;
        let p = poller(true, Some("test.jwt"), ok_synth()).await;
        assert_eq!(
            p.poll_once(&reqwest::Client::new(), &base).await,
            PollOutcome::Processed(1)
        );
        let g = rec.lock().await;
        assert!(g.result_calls[0].1["failure"]
            .as_str()
            .unwrap()
            .contains("kind"));
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
