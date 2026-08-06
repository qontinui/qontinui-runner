//! Tenant agentic-memory sync — outbox emitter + batch drain (plan
//! `2026-07-10-tenant-agentic-memory-web-backend`, Phase 2, runner side).
//!
//! ## What it does
//!
//! Local memory writers (task-run outcomes, reflection fixes, consolidated
//! mental models) call [`enqueue_memory_record`] to mirror a tenant-level
//! copy of each memory into the web backend's agentic-memory store. The
//! record rides the SAME local-first JSONL outbox pattern the session
//! cloud-sync substrate shipped ([`crate::session::local_store::OutboxWriter`])
//! — a dedicated `memory-outbox.jsonl` file with a new
//! [`SessionEventKind::MemoryRecord`] entry kind — and a drain loop batches
//! pending records (≤ [`MAX_BATCH`] per request, the server cap) to
//! `POST <web_base>/api/v1/memory/records` with the runner's coord-minted
//! device JWT as the bearer.
//!
//! ## Embeddings are computed HERE, not server-side
//!
//! Plan `2026-07-13-runner-paid-embedding` (Phase 1) moves the embed off the
//! web backend and onto the device that already has the model warm. The drain
//! batches the outgoing records' `content` through the local embedding service
//! ([`EmbeddingClient::compute_batch_embeddings`]) and stamps each record with
//! `embedding` (384 f32) + `embedding_model` ([`EMBEDDING_MODEL_TAG`]) before
//! the POST. The embed happens AFTER the consent + bearer gates, so a
//! non-consenting or unpaired runner never spends the compute either.
//!
//! **A local embedder outage must NEVER block a write.** `embedding` is
//! nullable by design: the backend writes a NULL-embedding row, which is
//! immediately retrievable via its FTS arm (degraded, not invisible) and gets
//! enqueued for later vectorization by the `kind="embedding"` job queue
//! ([`crate::memory::memory_synthesis`]). So when the embed fails, the drain
//! OMITS `embedding` + `embedding_model` and ships the records anyway. Making
//! the vector a hard requirement would turn a soft degradation into data loss:
//! a broken local embedding server would stall tenant memory sync on this
//! machine indefinitely and it would never federate.
//!
//! Contrast [`crate::memory::memory_synthesis`], where a claimed *job* IS left
//! un-resulted on an embedder outage. That is not the same situation and the
//! two must not be unified: a claimed job holds a lease the backend reaper
//! returns to `pending`, so deferring it loses nothing. A write holds no lease
//! — deferring it just queues local data forever.
//!
//! ## Anchors ride the same payload
//!
//! Plan `2026-07-29-memory-anchored-derived-records` (Phase 2, runner leg)
//! adds [`TenantMemoryRecord::anchors`] — typed references to the ground truth
//! a record asserts something about ([`MemoryAnchor`]). The emitter stamps the
//! array into the outbox payload at enqueue time, so the drain carries it to
//! `POST /api/v1/memory/records` with no further handling. It is ALWAYS
//! written, `[]` when empty, because the backend column is `NOT NULL DEFAULT
//! '[]'::jsonb`.
//!
//! The sibling column `anchor_state` is **writer-inaccessible**: it is derived
//! by coord's anchor watcher and the backend answers `422` to any writer that
//! supplies it. The runner therefore never sends it, and there is no field for
//! it on the outbound type.
//!
//! ## Gates + posture
//!
//! 1. **Consent gate (hard)** — `Settings.cloud_sync_enabled` (default
//!    false). Checked BEFORE anything else: with the toggle off,
//!    [`enqueue_memory_record`] returns without writing to the outbox (or
//!    even materializing it), so nothing leaves the machine.
//! 2. **Redaction** — every record's title + content pass through the shared
//!    [`crate::session::redact`] sweep BEFORE the durable outbox write, so a
//!    planted secret never persists locally, let alone egresses.
//! 3. **Offline tolerance** — records persist in the JSONL outbox and flush
//!    on recovery; the web side dedups by content hash, so a replay after a
//!    partial failure is a no-op (`deduped: true`).
//! 4. **429 quota** — a `memory_quota_exceeded` (or any other) 429 backs the
//!    drain off exponentially WITHOUT dropping or acking the batch; it
//!    retries later.
//! 5. **Unpaired / unconfigured** — no resolvable web base or no device JWT
//!    disables the drain with one warn per process; enqueued records stay
//!    pending locally. Never a panic.
//!
//! ## Wiring
//!
//! The process-global [`enqueue_memory_record`] lazy-initializes the sync
//! (outbox open + drain spawn) on first consented use, so no `main.rs`
//! setup edit is required and non-runner binaries (CLI tools, tests) never
//! touch the outbox. All production writers run inside the Tauri tokio
//! runtime, so the drain task always has a runtime to spawn onto.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{json, Value as JsonValue};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::database::embedding_client::{EmbeddingClient, EMBEDDING_MODEL_TAG};
use crate::session::local_store::{OutboxRecord, OutboxWriter};
use crate::session::redact::redact_secrets;
use crate::session::SessionEventKind;

/// Wire value of the outbox entry kind. Matches
/// [`SessionEventKind::MemoryRecord`]`.as_str()`.
pub const MEMORY_RECORD_EVENT: &str = "memory_record";

/// Server-side batch cap (`MAX_RECORDS_PER_REQUEST` in
/// `qontinui-web/backend/app/schemas/memory.py`).
pub const MAX_BATCH: usize = 100;

/// Server-side per-record content cap (bytes of UTF-8).
pub const MAX_CONTENT_BYTES: usize = 32 * 1024;

/// Server-side title cap (characters).
pub const MAX_TITLE_CHARS: usize = 512;

/// Server-side cap on anchors per record.
///
/// **Keep in lockstep with `MAX_ANCHORS_PER_RECORD` in
/// `qontinui-web/backend/app/schemas/memory.py`.** Lowering it there without
/// lowering it here costs whole batches — see [`sanitize_anchors`] for why.
pub const MAX_ANCHORS: usize = 16;

/// Per-field caps mirroring the `Field(max_length=…)` on each anchor variant
/// in `qontinui-web/backend/app/schemas/memory.py`. Pydantic's `max_length`
/// on a `str` counts CHARACTERS, so these are compared in `chars()`, the same
/// unit as [`MAX_TITLE_CHARS`] — not bytes.
pub const MAX_ANCHOR_REPO_CHARS: usize = 256;
/// See [`MAX_ANCHOR_REPO_CHARS`].
pub const MAX_ANCHOR_PATH_CHARS: usize = 1024;
/// See [`MAX_ANCHOR_REPO_CHARS`].
pub const MAX_ANCHOR_SHA_CHARS: usize = 64;
/// See [`MAX_ANCHOR_REPO_CHARS`].
pub const MAX_ANCHOR_REVISION_CHARS: usize = 256;
/// See [`MAX_ANCHOR_REPO_CHARS`].
pub const MAX_ANCHOR_OBJECT_CHARS: usize = 512;
/// See [`MAX_ANCHOR_REPO_CHARS`].
pub const MAX_ANCHOR_NAME_CHARS: usize = 256;

/// Memory kinds accepted by the web API (mirrors the `MemoryKind` literal +
/// the `coord_memory_records` CHECK constraint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRecordKind {
    Observation,
    Fact,
    MentalModel,
    Episode,
    Feedback,
    Reference,
    Rule,
}

impl MemoryRecordKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryRecordKind::Observation => "observation",
            MemoryRecordKind::Fact => "fact",
            MemoryRecordKind::MentalModel => "mental_model",
            MemoryRecordKind::Episode => "episode",
            MemoryRecordKind::Feedback => "feedback",
            MemoryRecordKind::Reference => "reference",
            MemoryRecordKind::Rule => "rule",
        }
    }
}

/// One typed reference to the ground truth a memory record asserts something
/// about (plan `2026-07-29-memory-anchored-derived-records`, §3.1). Each
/// variant is chosen because coord can already resolve it over a seam it
/// operates today, so a record's truth can be invalidated by the artifact
/// rather than decayed by the clock.
///
/// The `#[serde(tag = "type")]` shape is the wire contract: it must match the
/// backend's Pydantic discriminated union on `MemoryRecordIn.anchors`
/// byte-for-byte, e.g. `{"type":"pr","repo":"qontinui-runner","number":832}`.
///
/// **Five variants, deliberately — there is no `symbol`.** It was cut in
/// vetting: coord's `symbol_claims` are coordination claims about who is
/// *editing* a symbol, not a symbol index, and coord has no parser for any
/// fleet language, so the resolver would be larger than the rest of the plan.
/// A record wanting symbol granularity anchors the [`MemoryAnchor::Blob`].
/// Add a variant when a record needs it, **with** a coord-side resolver.
///
/// `anchor_state` is deliberately absent and must stay absent: it is the
/// watcher's derived roll-up (`none`/`fresh`/`moved`/`gone`), writer-
/// inaccessible by design, and the backend answers `422` to any writer that
/// supplies it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MemoryAnchor {
    /// A file at a git blob sha. `GET /repos/{repo}/contents/{path}` returns
    /// that sha in the same response, so resolving costs one existing call.
    Blob {
        repo: String,
        path: String,
        sha: String,
    },
    /// A pull request, resolved against coord's PR twin.
    Pr { repo: String, number: u64 },
    /// An alembic revision, resolved against `coord.migration_revisions`.
    Migration { revision: String },
    /// A schema object (`coord.memory_records.access_count`), resolved via the
    /// `coord_query_schema_object` path.
    Schema { object: String },
    /// A catalogued feature flag, resolved against coord's flag registry.
    Flag { name: String },
}

impl MemoryAnchor {
    /// Why the backend would reject this anchor with a `422`, or `None` when
    /// it is acceptable. Mirrors the per-field `Field(min_length=1,
    /// max_length=…)` / `Field(ge=1)` constraints on the Pydantic union in
    /// `qontinui-web/backend/app/schemas/memory.py`.
    ///
    /// Deliberately the SAME predicate as the server's, not a stricter one: a
    /// runner that rejected more than the backend would silently withhold
    /// anchors the store would have accepted.
    fn rejection_reason(&self) -> Option<String> {
        fn text(field: &str, value: &str, max: usize) -> Option<String> {
            if value.is_empty() {
                return Some(format!(
                    "`{field}` is empty (backend requires min_length=1)"
                ));
            }
            // Characters, not bytes — Pydantic's `max_length` counts code
            // points, so a byte-length check would reject valid non-ASCII.
            let n = value.chars().count();
            if n > max {
                return Some(format!("`{field}` is {n} chars (backend max {max})"));
            }
            None
        }
        match self {
            MemoryAnchor::Blob { repo, path, sha } => text("repo", repo, MAX_ANCHOR_REPO_CHARS)
                .or_else(|| text("path", path, MAX_ANCHOR_PATH_CHARS))
                .or_else(|| text("sha", sha, MAX_ANCHOR_SHA_CHARS)),
            MemoryAnchor::Pr { repo, number } => {
                text("repo", repo, MAX_ANCHOR_REPO_CHARS).or_else(|| {
                    (*number == 0).then(|| "`number` is 0 (backend requires ge=1)".to_string())
                })
            }
            MemoryAnchor::Migration { revision } => {
                text("revision", revision, MAX_ANCHOR_REVISION_CHARS)
            }
            MemoryAnchor::Schema { object } => text("object", object, MAX_ANCHOR_OBJECT_CHARS),
            MemoryAnchor::Flag { name } => text("name", name, MAX_ANCHOR_NAME_CHARS),
        }
    }
}

/// Drop anchors the backend would reject, and clamp the array to
/// [`MAX_ANCHORS`].
///
/// **A rejected anchor costs the whole BATCH, not just its record.** The
/// backend answers a malformed record with `422`, and
/// [`TenantMemorySync::flush_once`] classifies any non-429 4xx as permanent
/// and ack-drops the entire batch (up to [`MAX_BATCH`] records) so the queue
/// can move. So one empty string or a 17th anchor would silently destroy up to
/// 100 unrelated memory records. Filtering here makes that unreachable.
///
/// Dropping beats clamping for the string fields: a truncated `sha` or `path`
/// is a *wrong* anchor that the watcher would resolve to the wrong artifact
/// (or mark `gone`), which is worse than an absent one. Only the array LENGTH
/// is clamped. Every drop is logged at warn — losing an anchor is otherwise
/// silent, and this is the only place it becomes visible.
fn sanitize_anchors(anchors: Vec<MemoryAnchor>) -> Vec<MemoryAnchor> {
    let mut kept: Vec<MemoryAnchor> = Vec::with_capacity(anchors.len().min(MAX_ANCHORS));
    for anchor in anchors {
        if let Some(reason) = anchor.rejection_reason() {
            tracing::warn!(
                %reason,
                anchor = ?anchor,
                "tenant_sync: dropping an invalid anchor — shipping it would 422 the \
                 whole batch, which the drain ack-drops"
            );
            continue;
        }
        if kept.len() >= MAX_ANCHORS {
            tracing::warn!(
                cap = MAX_ANCHORS,
                "tenant_sync: anchor list exceeds the server cap — dropping the overflow"
            );
            break;
        }
        kept.push(anchor);
    }
    kept
}

/// One tenant-memory record as produced by a local writer. Scope defaults
/// server-side to `tenant`; the runner only writes tenant-scoped copies.
#[derive(Debug, Clone)]
pub struct TenantMemoryRecord {
    pub title: String,
    pub content: String,
    pub kind: MemoryRecordKind,
    /// 0.0–1.0; server default 0.5.
    pub importance: f64,
    /// Provenance JSON (`{device_id?, task_run_id?, repo?, …}`). The emitter
    /// stamps `device_id` automatically when absent.
    pub source: JsonValue,
    /// Ground truth this record's truth is owned by. Empty (the default) means
    /// an ordinary narrative record that keeps today's time-decay lifecycle;
    /// a non-empty array makes the record decay-exempt and invalidated by the
    /// coord anchor watcher instead. Always serialized — the backend column is
    /// `NOT NULL DEFAULT '[]'::jsonb`, so this ships `[]`, never `null`.
    pub anchors: Vec<MemoryAnchor>,
}

impl TenantMemoryRecord {
    pub fn new(
        title: impl Into<String>,
        content: impl Into<String>,
        kind: MemoryRecordKind,
    ) -> Self {
        Self {
            title: title.into(),
            content: content.into(),
            kind,
            importance: 0.5,
            source: json!({}),
            anchors: Vec::new(),
        }
    }

    pub fn with_importance(mut self, importance: f64) -> Self {
        self.importance = importance.clamp(0.0, 1.0);
        self
    }

    pub fn with_source(mut self, source: JsonValue) -> Self {
        self.source = source;
        self
    }

    /// Bind this record to the ground truth it asserts something about. See
    /// [`MemoryAnchor`] for why the vocabulary is closed at five types.
    ///
    /// Infallible, matching the module's posture that a memory write never
    /// fails its caller: anchors the backend would reject are dropped here
    /// (with a warn) rather than surfaced as an error, because the alternative
    /// — shipping them — costs the whole batch. See [`sanitize_anchors`].
    pub fn with_anchors(mut self, anchors: Vec<MemoryAnchor>) -> Self {
        self.anchors = sanitize_anchors(anchors);
        self
    }
}

/// Consent-gate probe (`Settings.cloud_sync_enabled`). Injected so tests
/// never touch the machine's real `settings.json` — same pattern as
/// [`crate::session::restore_record_emitter::ConsentGate`].
pub type ConsentGate = Box<dyn Fn() -> bool + Send + Sync>;

/// Device-JWT provider. Production reads the default binding's slot via
/// [`crate::auth::device_bearer`]; tests inject a fixed token.
pub type BearerProvider = Box<dyn Fn() -> Option<String> + Send + Sync>;

/// Outcome of one [`TenantMemorySync::flush_once`] pass. Drives the drain
/// loop's cadence/backoff; surfaced for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlushOutcome {
    /// Nothing pending.
    Idle,
    /// Batch accepted (2xx) and acked locally.
    Flushed(usize),
    /// 429 (quota / rate limit). Batch kept pending — retry after backoff.
    RateLimited,
    /// No device JWT available (unpaired). Batch kept pending.
    NoAuth,
    /// Transient failure (network, 5xx, 401/403 mid-token-refresh). Batch
    /// kept pending — retry after backoff. A local-embedder outage is NOT in
    /// this class: it ships un-embedded rather than deferring the write.
    Retry(String),
    /// Non-retryable 4xx — the server refuses this batch permanently. The
    /// batch is ack-dropped so the queue moves forward.
    Dropped(usize),
}

/// Tenant-memory sync facade: consent-gated, redacting outbox writer + the
/// batch drain against the web memory API.
pub struct TenantMemorySync {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    gate: ConsentGate,
    bearer: BearerProvider,
    /// Local embedding service client. Every outgoing record's vector is
    /// computed through this before the POST.
    embedder: EmbeddingClient,
    warned_no_auth: std::sync::Once,
}

impl std::fmt::Debug for TenantMemorySync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantMemorySync")
            .field("machine_id", &self.machine_id)
            .finish()
    }
}

impl TenantMemorySync {
    /// Production constructor: gate reads the real
    /// `Settings.cloud_sync_enabled`, bearer reads the default device-JWT
    /// slot.
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        Self::with_probes(
            outbox,
            machine_id,
            Box::new(crate::settings::get_cloud_sync_enabled),
            Box::new(crate::auth::device_bearer),
        )
    }

    /// Constructor with injectable consent gate + bearer provider so tests
    /// drive both deterministically without touching `settings.json` or the
    /// credential store.
    pub fn with_probes(
        outbox: Arc<OutboxWriter>,
        machine_id: Uuid,
        gate: ConsentGate,
        bearer: BearerProvider,
    ) -> Self {
        Self {
            outbox,
            machine_id,
            gate,
            bearer,
            embedder: EmbeddingClient::new(),
            warned_no_auth: std::sync::Once::new(),
        }
    }

    /// Point the drain at a specific embedding service (tests aim this at a
    /// local fake; production keeps [`EmbeddingClient::new`]'s default URL).
    pub fn with_embedder(mut self, embedder: EmbeddingClient) -> Self {
        self.embedder = embedder;
        self
    }

    /// Enqueue one tenant-memory record. **Consent gate 1 (hard):** when the
    /// gate is closed this is a NO-OP — nothing is written to the outbox and
    /// nothing can egress. Title + content are redacted BEFORE the durable
    /// write and clamped to the server caps. Never fails the caller.
    pub fn enqueue(&self, record: TenantMemoryRecord) {
        if !(self.gate)() {
            return;
        }

        let title = redact_text(&record.title);
        let title: String = title.trim().chars().take(MAX_TITLE_CHARS).collect();
        let content = redact_text(&record.content);
        let content = truncate_utf8(content.trim(), MAX_CONTENT_BYTES).to_string();
        if title.is_empty() || content.is_empty() {
            tracing::debug!("tenant_sync: skipping record with empty title/content");
            return;
        }

        // Stamp device provenance when the writer didn't set it.
        let mut source = record.source;
        if !source.is_object() {
            source = json!({ "value": source });
        }
        if source.get("device_id").is_none() {
            source["device_id"] = JsonValue::String(self.machine_id.to_string());
        }

        let payload = json!({
            "title": title,
            "content": content,
            "kind": record.kind.as_str(),
            "importance": record.importance.clamp(0.0, 1.0),
            "source": source,
            // Always present, `[]` when empty — the backend column is
            // `NOT NULL DEFAULT '[]'::jsonb` and a `null` would be rejected.
            // Anchors are structural references, never prose, so they bypass
            // the redaction sweep that title/content go through.
            //
            // Re-sanitized here, not just in `with_anchors`: `anchors` is a
            // pub field like every other on the record, so a writer can assign
            // it directly and bypass the builder. This is the last gate before
            // the durable outbox write, so it is the one that has to hold.
            // Idempotent, so paying it twice costs nothing.
            "anchors": sanitize_anchors(record.anchors),
        });

        if let Err(e) = self.outbox.record(
            self.machine_id,
            self.machine_id,
            SessionEventKind::MemoryRecord,
            payload,
        ) {
            tracing::warn!(
                error = %e,
                "tenant_sync: memory outbox append failed (best-effort) — record dropped locally"
            );
        }
    }

    /// Flush at most one batch of pending records to the web memory API.
    /// See [`FlushOutcome`] for the retry semantics per response class.
    pub(crate) async fn flush_once(&self, client: &reqwest::Client, base: &str) -> FlushOutcome {
        let pending = match self.outbox.pending() {
            Ok(p) => p,
            Err(e) => return FlushOutcome::Retry(format!("outbox pending() failed: {e}")),
        };
        let batch: Vec<OutboxRecord> = pending
            .into_iter()
            .filter(|r| r.event_kind == MEMORY_RECORD_EVENT)
            .take(MAX_BATCH)
            .collect();
        if batch.is_empty() {
            return FlushOutcome::Idle;
        }

        let Some(bearer) = (self.bearer)() else {
            self.warned_no_auth.call_once(|| {
                tracing::warn!(
                    "tenant_sync: no device JWT available — tenant-memory records stay \
                     pending locally until this runner is paired"
                );
            });
            return FlushOutcome::NoAuth;
        };

        // Embed locally (plan `2026-07-13-runner-paid-embedding`, Phase 1): the
        // backend no longer embeds, so a record without a vector is useless to
        // it. Runs after the consent + bearer gates so a gated-off runner never
        // spends the compute.
        let texts: Vec<String> = batch
            .iter()
            .map(|r| {
                r.payload
                    .get("content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        // `None` ⇒ ship un-embedded. `embedding` is nullable and the backend
        // enqueues NULL-embedding rows for later vectorization, so a local
        // embedder outage costs freshness of the vector arm — never the write.
        let embeddings: Option<Vec<Vec<f32>>> =
            match self.embedder.compute_batch_embeddings(&texts).await {
                Ok(e) if e.len() == batch.len() => Some(e),
                Ok(e) => {
                    // The embedding service broke its contract. Ship the batch
                    // un-embedded rather than pairing vectors to the wrong
                    // records — a mis-zipped vector is worse than no vector.
                    tracing::warn!(
                        got = e.len(),
                        want = batch.len(),
                        "tenant_sync: embedding service returned the wrong vector count — \
                         shipping records un-embedded for the backend to vectorize later"
                    );
                    None
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        batch = batch.len(),
                        "tenant_sync: local embed failed — shipping records un-embedded; the \
                         backend stores them NULL-embedding (FTS-retrievable) and enqueues \
                         them for later vectorization"
                    );
                    None
                }
            };

        let records: Vec<JsonValue> = batch
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let mut payload = r.payload.clone();
                if let Some(embeddings) = &embeddings {
                    payload["embedding"] = json!(embeddings[i]);
                    payload["embedding_model"] = JsonValue::String(EMBEDDING_MODEL_TAG.to_string());
                }
                payload
            })
            .collect();
        let url = format!("{}/api/v1/memory/records", base.trim_end_matches('/'));
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {bearer}"))
            .json(&json!({ "records": records }))
            .send()
            .await;

        match resp {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    if let Ok(body) = resp.json::<JsonValue>().await {
                        if let Some(deduped) = body.get("deduped_count").and_then(JsonValue::as_i64)
                        {
                            if deduped > 0 {
                                tracing::debug!(
                                    deduped,
                                    "tenant_sync: server deduped replayed records"
                                );
                            }
                        }
                    }
                    let acks: Vec<(Uuid, i64)> =
                        batch.iter().map(|r| (r.session_id, r.seq)).collect();
                    if let Err(e) = self.outbox.ack(&acks) {
                        tracing::warn!(error = %e, "tenant_sync: outbox ack failed");
                    }
                    return FlushOutcome::Flushed(batch.len());
                }
                if status == StatusCode::TOO_MANY_REQUESTS {
                    // Quota / rate limit — retrying immediately can't help;
                    // keep the batch pending and let the loop back off.
                    let detail = resp.text().await.unwrap_or_default();
                    tracing::info!(
                        batch = batch.len(),
                        detail = %truncate_utf8(&detail, 256),
                        "tenant_sync: memory write rate-limited (429) — backing off, not dropping"
                    );
                    return FlushOutcome::RateLimited;
                }
                if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                    // The device JWT refreshes in place (4h TTL); a 401/403
                    // is usually a mid-refresh blip — transient.
                    return FlushOutcome::Retry(format!("auth rejected ({status})"));
                }
                let detail = resp.text().await.unwrap_or_default();
                if status.is_client_error() {
                    // The server refuses this batch permanently (validation).
                    // Ack-drop so the queue moves forward — a malformed
                    // record must not hold every later record hostage.
                    tracing::error!(
                        %status,
                        detail = %truncate_utf8(&detail, 512),
                        batch = batch.len(),
                        "tenant_sync: memory write permanently rejected — dropping batch"
                    );
                    let acks: Vec<(Uuid, i64)> =
                        batch.iter().map(|r| (r.session_id, r.seq)).collect();
                    if let Err(e) = self.outbox.ack(&acks) {
                        tracing::warn!(error = %e, "tenant_sync: outbox ack failed");
                    }
                    return FlushOutcome::Dropped(batch.len());
                }
                FlushOutcome::Retry(format!("{status}: {}", truncate_utf8(&detail, 256)))
            }
            Err(e) => FlushOutcome::Retry(format!("transport: {e}")),
        }
    }

    /// Start the drain task. Offline-tolerant: transient failures back off
    /// exponentially (capped) and the JSONL file is the queue.
    pub fn start_drain_task(self: &Arc<Self>) -> JoinHandle<()> {
        let sync = Arc::clone(self);
        tokio::spawn(run_drain_loop(sync))
    }
}

/// Idle cadence when the outbox is empty (or the runner is unpaired).
const TICK_IDLE: Duration = Duration::from_secs(30);
/// Cadence between successive non-empty batches.
const TICK_BUSY: Duration = Duration::from_secs(1);
/// Initial retry backoff.
const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
/// Backoff ceiling (also applied to 429s — quota frees slowly).
const BACKOFF_MAX: Duration = Duration::from_secs(300);

async fn run_drain_loop(sync: Arc<TenantMemorySync>) {
    tracing::info!("tenant_sync: memory drain loop starting");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "tenant_sync: reqwest client build failed; using default");
            reqwest::Client::new()
        });
    let warned_no_base = std::sync::Once::new();
    let mut backoff = BACKOFF_INITIAL;
    loop {
        let Some(base) = resolve_web_base() else {
            warned_no_base.call_once(|| {
                tracing::warn!(
                    "tenant_sync: no web backend base resolvable (QONTINUI_WEB_BASE unset and \
                     no profile coord_url) — tenant-memory emitter idle until configured"
                );
            });
            tokio::time::sleep(TICK_IDLE).await;
            continue;
        };
        match sync.flush_once(&client, &base).await {
            FlushOutcome::Idle | FlushOutcome::NoAuth => {
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_IDLE).await;
            }
            FlushOutcome::Flushed(n) => {
                tracing::debug!(count = n, "tenant_sync: flushed memory records");
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_BUSY).await;
            }
            FlushOutcome::Dropped(_) => {
                backoff = BACKOFF_INITIAL;
                tokio::time::sleep(TICK_BUSY).await;
            }
            FlushOutcome::RateLimited => {
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, BACKOFF_MAX);
            }
            FlushOutcome::Retry(reason) => {
                tracing::debug!(%reason, "tenant_sync: flush failed; will retry");
                tokio::time::sleep(backoff).await;
                backoff = std::cmp::min(backoff * 2, BACKOFF_MAX);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process-global emitter (lazy-initialized on first consented use)
// ---------------------------------------------------------------------------

static GLOBAL: OnceLock<Option<Arc<TenantMemorySync>>> = OnceLock::new();

/// Enqueue a tenant-memory record via the process-global emitter.
///
/// - Consent gate 1 (hard): with `cloud_sync_enabled` off this returns
///   immediately — the sync is never even initialized, no file is created,
///   nothing egresses.
/// - First consented call lazy-initializes the outbox + drain loop (all
///   production writers run inside the Tauri tokio runtime).
/// - Never fails or panics; every failure mode collapses to a log line.
pub fn enqueue_memory_record(record: TenantMemoryRecord) {
    if !crate::settings::get_cloud_sync_enabled() {
        return;
    }
    if let Some(sync) = global() {
        sync.enqueue(record);
    }
}

fn global() -> Option<Arc<TenantMemorySync>> {
    GLOBAL
        .get_or_init(|| {
            let sync = init_global()?;
            let sync = Arc::new(sync);
            match tokio::runtime::Handle::try_current() {
                Ok(_) => {
                    let _drain = sync.start_drain_task();
                }
                Err(_) => {
                    tracing::warn!(
                        "tenant_sync: no tokio runtime at init — records will queue locally \
                         without draining in this process"
                    );
                }
            }
            Some(sync)
        })
        .clone()
}

/// Open the dedicated memory outbox + resolve the device identity. `None`
/// (with a warn) when even the temp-dir fallback fails — enqueue then
/// degrades to a no-op rather than panicking.
fn init_global() -> Option<TenantMemorySync> {
    // Same identity resolution as `main.rs` setup: machine.json `device_id`
    // (legacy `machine_id` alias), else a per-process UUID.
    let machine_id = dirs::home_dir()
        .and_then(|h| std::fs::read(h.join(".qontinui").join("machine.json")).ok())
        .and_then(|b| {
            let v: JsonValue = serde_json::from_slice(&b).ok()?;
            let s = v
                .get("device_id")
                .and_then(|x| x.as_str())
                .or_else(|| v.get("machine_id").and_then(|x| x.as_str()))?;
            Uuid::parse_str(s).ok()
        })
        .unwrap_or_else(Uuid::new_v4);

    // Instance-scoped like the session outbox so temp/named runners never
    // race the primary on one file.
    let dir = crate::instance::scope_path(
        &dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".qontinui")
            .join("runner"),
    );
    let path = dir.join("memory-outbox.jsonl");
    let outbox = match OutboxWriter::open(&path) {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "tenant_sync: memory outbox open failed — using ephemeral fallback"
            );
            let fallback = std::env::temp_dir().join("qontinui-runner-memory-outbox.jsonl");
            match OutboxWriter::open(&fallback) {
                Ok(o) => o,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "tenant_sync: ephemeral memory outbox open failed — emitter disabled"
                    );
                    return None;
                }
            }
        }
    };
    Some(TenantMemorySync::new(Arc::new(outbox), machine_id))
}

/// Resolve the web backend base URL the memory API lives behind.
///
/// Order: `QONTINUI_WEB_BASE` env (temp-runner / test override) → the
/// operator-configured `web_integration.backend_url` from settings → the
/// coord-derived fallback. `None` only when all three are absent.
///
/// The `web_integration.backend_url` step is load-bearing on the PRIMARY: it
/// is the SAME base the runner's WS relay + `/api/v1/*` calls already use
/// (e.g. `https://api.qontinui.io`). Without it, resolution fell straight
/// through to `resolve_backend_base`'s coord-derivation, whose
/// `derive_web_base_from_coord` strips a trailing `:port` and so mangles a
/// PORTLESS production coord URL — `https://coord.qontinui.io` → `"https"`.
/// That left the primary (which sets no `QONTINUI_WEB_BASE`) unable to reach
/// the backend to upload memory records OR drain the synthesis/embedding job
/// queues; only temp runners with an explicit `QONTINUI_WEB_BASE` ever worked.
pub(crate) fn resolve_web_base() -> Option<String> {
    if let Ok(v) = std::env::var("QONTINUI_WEB_BASE") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.trim_end_matches('/').to_string());
        }
    }
    let wi = crate::settings::load_settings().web_integration;
    if wi.enabled {
        let b = wi.backend_url.trim();
        if !b.is_empty() {
            return Some(b.trim_end_matches('/').to_string());
        }
    }
    qontinui_runner_lib::env_agent::enroll::resolve_backend_base(None).ok()
}

fn redact_text(s: &str) -> String {
    String::from_utf8(redact_secrets(s.as_bytes()))
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Truncate to at most `max_bytes` bytes on a char boundary.
fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::State as AxumState, http::HeaderMap, http::StatusCode as AxumStatus,
        response::IntoResponse, routing::post, Json, Router,
    };
    use std::sync::Arc;
    use tempfile::tempdir;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokMutex;

    /// Enqueue-only sync. The embedder is never reached (enqueue does not
    /// embed), so these tests stay hermetic without a fake embedding service.
    fn make_sync(
        dir: &std::path::Path,
        gate_open: bool,
        bearer: Option<&'static str>,
    ) -> (TenantMemorySync, Arc<OutboxWriter>) {
        let outbox = Arc::new(OutboxWriter::open(dir.join("memory-outbox.jsonl")).unwrap());
        let sync = TenantMemorySync::with_probes(
            outbox.clone(),
            Uuid::new_v4(),
            Box::new(move || gate_open),
            Box::new(move || bearer.map(str::to_string)),
        );
        (sync, outbox)
    }

    /// A flushing sync wired to `embed_base`'s fake embedding service. Point
    /// `embed_base` at an unroutable host to exercise the embedder-down path.
    fn make_flushing_sync(
        dir: &std::path::Path,
        bearer: Option<&'static str>,
        embed_base: &str,
    ) -> (TenantMemorySync, Arc<OutboxWriter>) {
        let (sync, outbox) = make_sync(dir, true, bearer);
        let sync = sync.with_embedder(EmbeddingClient::with_url(&format!(
            "{}/api/embeddings/compute-text",
            embed_base.trim_end_matches('/')
        )));
        (sync, outbox)
    }

    /// Deterministic stand-in for the local embedding service. Serves the
    /// batch route `EmbeddingClient` derives (`compute-text` → `compute-batch`)
    /// and records the texts it was asked to embed, so a test can assert
    /// exactly WHAT egressed to the embedder.
    async fn spawn_fake_embedder() -> (String, Arc<TokMutex<Vec<String>>>) {
        let seen: Arc<TokMutex<Vec<String>>> = Arc::new(TokMutex::new(Vec::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/api/embeddings/compute-batch",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<Vec<String>>>>,
                     Json(body): Json<JsonValue>| async move {
                        let texts: Vec<String> = body["texts"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|t| t.as_str().unwrap_or_default().to_string())
                                    .collect()
                            })
                            .unwrap_or_default();
                        // One distinct 384-dim vector per text, keyed by index
                        // so a test can prove per-record alignment.
                        let embeddings: Vec<Vec<f32>> = texts
                            .iter()
                            .enumerate()
                            .map(|(i, _)| vec![i as f32; 384])
                            .collect();
                        state.lock().await.extend(texts);
                        Json(json!({ "embeddings": embeddings }))
                    },
                ),
            )
            .with_state(seen.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{}", addr), seen)
    }

    fn record(title: &str, content: &str) -> TenantMemoryRecord {
        TenantMemoryRecord::new(title, content, MemoryRecordKind::Episode)
            .with_importance(0.7)
            .with_source(json!({"task_run_id": "tr-1"}))
    }

    /// Everything the fake web server saw, plus the response it should give.
    #[derive(Default)]
    struct WebRecorder {
        bodies: Vec<JsonValue>,
        auth_headers: Vec<Option<String>>,
        respond_status: Option<u16>,
    }

    async fn spawn_fake_web(respond_status: Option<u16>) -> (String, Arc<TokMutex<WebRecorder>>) {
        let rec = Arc::new(TokMutex::new(WebRecorder {
            respond_status,
            ..WebRecorder::default()
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app: Router = Router::new()
            .route(
                "/api/v1/memory/records",
                post(
                    |AxumState(state): AxumState<Arc<TokMutex<WebRecorder>>>,
                     headers: HeaderMap,
                     Json(body): Json<JsonValue>| async move {
                        let mut g = state.lock().await;
                        g.auth_headers.push(
                            headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_string),
                        );
                        g.bodies.push(body.clone());
                        if let Some(status) = g.respond_status {
                            return (
                                AxumStatus::from_u16(status).unwrap(),
                                Json(json!({"error": "memory_quota_exceeded"})),
                            )
                                .into_response();
                        }
                        let n = body
                            .get("records")
                            .and_then(JsonValue::as_array)
                            .map(Vec::len)
                            .unwrap_or(0);
                        let results: Vec<JsonValue> = (0..n)
                            .map(|_| json!({"memory_id": Uuid::new_v4(), "deduped": false}))
                            .collect();
                        (
                            AxumStatus::OK,
                            Json(json!({"records": results, "deduped_count": 0})),
                        )
                            .into_response()
                    },
                ),
            )
            .with_state(rec.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{}", addr), rec)
    }

    #[test]
    fn consent_gate_off_is_a_hard_noop() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), false, Some("test.jwt"));
        sync.enqueue(record("Task succeeded", "did the thing"));
        assert!(
            outbox.pending().unwrap().is_empty(),
            "cloud_sync_enabled off ⇒ nothing is written to the outbox"
        );
    }

    #[test]
    fn redaction_runs_before_enqueue() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
        // Assembled at runtime so source-level secret scanning never sees a
        // key=value adjacency.
        let content = format!(
            "set API_KEY={} then password: {}",
            "sk-live-4242", "hunter2"
        );
        let title = format!("token={} rotated", "ghp_fake987");
        sync.enqueue(TenantMemoryRecord::new(
            title,
            content,
            MemoryRecordKind::Fact,
        ));

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let payload = pending[0].payload.to_string();
        assert!(
            !payload.contains("sk-live-4242"),
            "secret leaked: {payload}"
        );
        assert!(!payload.contains("hunter2"), "secret leaked: {payload}");
        assert!(!payload.contains("ghp_fake987"), "secret leaked: {payload}");
        assert!(
            payload.contains(crate::session::redact::MASK),
            "mask absent: {payload}"
        );
    }

    #[test]
    fn enqueue_stamps_device_id_and_clamps_caps() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
        let long_title = "t".repeat(MAX_TITLE_CHARS + 100);
        let long_content = "c".repeat(MAX_CONTENT_BYTES + 100);
        sync.enqueue(TenantMemoryRecord::new(
            long_title,
            long_content,
            MemoryRecordKind::Observation,
        ));

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let p = &pending[0].payload;
        assert_eq!(
            p["title"].as_str().unwrap().chars().count(),
            MAX_TITLE_CHARS
        );
        assert_eq!(p["content"].as_str().unwrap().len(), MAX_CONTENT_BYTES);
        assert!(
            p["source"]["device_id"].as_str().is_some(),
            "emitter must stamp device provenance"
        );
        assert_eq!(p["kind"], "observation");
    }

    /// The anchor vocabulary is a WIRE CONTRACT with the backend's Pydantic
    /// discriminated union (`MemoryRecordIn.anchors`). Pin the exact JSON of
    /// every variant — a serde attribute drift here surfaces as a `422` batch
    /// drop in production, which the drain ack-drops silently.
    #[test]
    fn anchor_variants_serialize_to_the_backend_discriminated_union() {
        let cases: Vec<(MemoryAnchor, JsonValue)> = vec![
            (
                MemoryAnchor::Blob {
                    repo: "qontinui-web".into(),
                    path: "backend/app/services/memory_store.py".into(),
                    sha: "e3b0c44298fc1c149afbf4c8996fb924".into(),
                },
                json!({
                    "type": "blob",
                    "repo": "qontinui-web",
                    "path": "backend/app/services/memory_store.py",
                    "sha": "e3b0c44298fc1c149afbf4c8996fb924",
                }),
            ),
            (
                MemoryAnchor::Pr {
                    repo: "qontinui-runner".into(),
                    number: 832,
                },
                json!({"type": "pr", "repo": "qontinui-runner", "number": 832}),
            ),
            (
                MemoryAnchor::Migration {
                    revision: "coord_memory_links".into(),
                },
                json!({"type": "migration", "revision": "coord_memory_links"}),
            ),
            (
                MemoryAnchor::Schema {
                    object: "coord.memory_records.access_count".into(),
                },
                json!({"type": "schema", "object": "coord.memory_records.access_count"}),
            ),
            (
                MemoryAnchor::Flag {
                    name: "merge_rollout".into(),
                },
                json!({"type": "flag", "name": "merge_rollout"}),
            ),
        ];
        assert_eq!(
            cases.len(),
            5,
            "the vocabulary is five types — `symbol` was cut in vetting because coord \
             has no symbol index; a sixth type needs a coord-side resolver, not just \
             a variant here"
        );
        for (anchor, want) in cases {
            assert_eq!(
                serde_json::to_value(&anchor).unwrap(),
                want,
                "wire shape drift for {anchor:?}"
            );
        }

        // `anchor_state` is the watcher's derived roll-up — the backend 422s a
        // writer that supplies it, so it must appear nowhere on this type.
        let all = json!(vec![MemoryAnchor::Flag {
            name: "merge_rollout".into()
        }])
        .to_string();
        assert!(
            !all.contains("anchor_state"),
            "the runner must never send anchor_state: {all}"
        );

        // NOT `null` — the backend column is `NOT NULL DEFAULT '[]'::jsonb`.
        let empty: Vec<MemoryAnchor> = Vec::new();
        assert_eq!(serde_json::to_value(&empty).unwrap(), json!([]));
    }

    /// A valid anchor must survive validation completely untouched — the whole
    /// point of dropping rather than clamping is that what ships is either the
    /// author's exact anchor or nothing.
    #[test]
    fn valid_anchors_round_trip_through_validation_unchanged() {
        let valid = vec![
            MemoryAnchor::Blob {
                repo: "qontinui-web".into(),
                path: "backend/app/services/memory_store.py".into(),
                sha: "e3b0c44298fc1c149afbf4c8996fb924".into(),
            },
            MemoryAnchor::Pr {
                repo: "qontinui-runner".into(),
                number: 832,
            },
            MemoryAnchor::Migration {
                revision: "coord_memory_links".into(),
            },
            MemoryAnchor::Schema {
                object: "coord.memory_records.access_count".into(),
            },
            MemoryAnchor::Flag {
                name: "merge_rollout".into(),
            },
        ];
        let record = TenantMemoryRecord::new("t", "c", MemoryRecordKind::Reference)
            .with_anchors(valid.clone());
        assert_eq!(
            record.anchors, valid,
            "validation must not mutate or drop a valid anchor set"
        );
        // …and still serializes to the exact wire shape pinned above.
        assert_eq!(
            serde_json::to_value(&record.anchors).unwrap()[1],
            json!({"type": "pr", "repo": "qontinui-runner", "number": 832})
        );

        // Boundary values are VALID, not rejected: exactly at each cap.
        let at_cap = vec![
            MemoryAnchor::Blob {
                repo: "r".repeat(MAX_ANCHOR_REPO_CHARS),
                path: "p".repeat(MAX_ANCHOR_PATH_CHARS),
                sha: "s".repeat(MAX_ANCHOR_SHA_CHARS),
            },
            MemoryAnchor::Pr {
                repo: "r".into(),
                number: 1,
            },
            MemoryAnchor::Schema {
                object: "o".repeat(MAX_ANCHOR_OBJECT_CHARS),
            },
        ];
        assert_eq!(
            sanitize_anchors(at_cap.clone()),
            at_cap,
            "the caps are inclusive — an anchor exactly at the limit is valid"
        );
    }

    /// Every constraint the backend enforces, enforced here FIRST. This is not
    /// a validation nit: a `422` makes `flush_once` ack-drop the whole batch,
    /// so one bad anchor would silently destroy up to `MAX_BATCH` unrelated
    /// records on the memory write path.
    #[test]
    fn invalid_anchors_are_dropped_before_they_can_422_the_batch() {
        let ok = MemoryAnchor::Flag {
            name: "merge_rollout".into(),
        };
        let rejected = vec![
            (
                "empty blob repo",
                MemoryAnchor::Blob {
                    repo: String::new(),
                    path: "a/b.py".into(),
                    sha: "abc".into(),
                },
            ),
            (
                "empty blob path",
                MemoryAnchor::Blob {
                    repo: "qontinui-web".into(),
                    path: String::new(),
                    sha: "abc".into(),
                },
            ),
            (
                "empty blob sha",
                MemoryAnchor::Blob {
                    repo: "qontinui-web".into(),
                    path: "a/b.py".into(),
                    sha: String::new(),
                },
            ),
            (
                "sha over 64 chars",
                MemoryAnchor::Blob {
                    repo: "qontinui-web".into(),
                    path: "a/b.py".into(),
                    sha: "s".repeat(MAX_ANCHOR_SHA_CHARS + 1),
                },
            ),
            (
                "path over cap",
                MemoryAnchor::Blob {
                    repo: "qontinui-web".into(),
                    path: "p".repeat(MAX_ANCHOR_PATH_CHARS + 1),
                    sha: "abc".into(),
                },
            ),
            (
                "repo over cap",
                MemoryAnchor::Blob {
                    repo: "r".repeat(MAX_ANCHOR_REPO_CHARS + 1),
                    path: "a/b.py".into(),
                    sha: "abc".into(),
                },
            ),
            (
                "empty pr repo",
                MemoryAnchor::Pr {
                    repo: String::new(),
                    number: 832,
                },
            ),
            (
                "pr number 0",
                MemoryAnchor::Pr {
                    repo: "qontinui-runner".into(),
                    number: 0,
                },
            ),
            (
                "empty revision",
                MemoryAnchor::Migration {
                    revision: String::new(),
                },
            ),
            (
                "revision over cap",
                MemoryAnchor::Migration {
                    revision: "r".repeat(MAX_ANCHOR_REVISION_CHARS + 1),
                },
            ),
            (
                "empty object",
                MemoryAnchor::Schema {
                    object: String::new(),
                },
            ),
            (
                "object over cap",
                MemoryAnchor::Schema {
                    object: "o".repeat(MAX_ANCHOR_OBJECT_CHARS + 1),
                },
            ),
            (
                "empty name",
                MemoryAnchor::Flag {
                    name: String::new(),
                },
            ),
            (
                "name over cap",
                MemoryAnchor::Flag {
                    name: "n".repeat(MAX_ANCHOR_NAME_CHARS + 1),
                },
            ),
        ];
        for (label, bad) in rejected {
            assert!(
                bad.rejection_reason().is_some(),
                "{label} must be rejected: {bad:?}"
            );
            // The valid neighbour survives — one bad anchor drops itself, not
            // the record's other anchors.
            assert_eq!(
                sanitize_anchors(vec![bad, ok.clone()]),
                vec![ok.clone()],
                "{label}: only the invalid anchor may be dropped"
            );
        }
    }

    /// The array cap is clamped, not rejected — mirrors
    /// `MAX_ANCHORS_PER_RECORD` in `qontinui-web/backend/app/schemas/memory.py`.
    #[test]
    fn anchor_list_is_clamped_to_the_server_cap() {
        assert_eq!(MAX_ANCHORS, 16, "must track MAX_ANCHORS_PER_RECORD");
        let many: Vec<MemoryAnchor> = (0..MAX_ANCHORS + 4)
            .map(|i| MemoryAnchor::Flag {
                name: format!("flag_{i}"),
            })
            .collect();
        let kept = sanitize_anchors(many.clone());
        assert_eq!(kept.len(), MAX_ANCHORS, "a 17th anchor would 422 the batch");
        assert_eq!(
            kept.as_slice(),
            &many[..MAX_ANCHORS],
            "the clamp keeps the FIRST anchors in order, not an arbitrary subset"
        );

        // Invalid entries are dropped before the cap is counted, so a full 16
        // valid anchors still all make it in when an earlier one was rejected.
        let mut mixed = vec![MemoryAnchor::Flag {
            name: String::new(),
        }];
        mixed.extend(many.iter().take(MAX_ANCHORS).cloned());
        assert_eq!(sanitize_anchors(mixed).len(), MAX_ANCHORS);
    }

    /// `anchors` is a `pub` field, so a writer can assign it directly and skip
    /// `with_anchors`. `enqueue` is the last gate before the durable write, so
    /// it must re-validate — otherwise the builder is only a suggestion.
    #[test]
    fn enqueue_revalidates_anchors_assigned_around_the_builder() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
        let mut smuggled = record("bypass", "assigned the field directly");
        smuggled.anchors = vec![
            MemoryAnchor::Pr {
                repo: "qontinui-runner".into(),
                number: 0,
            },
            MemoryAnchor::Flag {
                name: "merge_rollout".into(),
            },
        ];
        sync.enqueue(smuggled);

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].payload["anchors"],
            json!([{"type": "flag", "name": "merge_rollout"}]),
            "an anchor that bypassed the builder must still be filtered before \
             it reaches the outbox"
        );
    }

    /// The enqueued payload is what the drain ships verbatim, so the anchors
    /// must be stamped at enqueue time — and defaulted to `[]`, never absent
    /// or `null`, for every existing writer that sets none.
    #[test]
    fn enqueue_carries_anchors_and_defaults_to_an_empty_array() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
        sync.enqueue(record("plain", "no anchors here"));
        sync.enqueue(
            record("anchored", "asserts something about a file").with_anchors(vec![
                MemoryAnchor::Blob {
                    repo: "qontinui-runner".into(),
                    path: "src-tauri/src/memory/tenant_sync.rs".into(),
                    sha: "deadbeef".into(),
                },
                MemoryAnchor::Flag {
                    name: "merge_rollout".into(),
                },
            ]),
        );

        let pending = outbox.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending[0].payload["anchors"],
            json!([]),
            "an anchorless writer must ship `[]`, not null and not absent"
        );
        assert_eq!(
            pending[1].payload["anchors"],
            json!([
                {
                    "type": "blob",
                    "repo": "qontinui-runner",
                    "path": "src-tauri/src/memory/tenant_sync.rs",
                    "sha": "deadbeef",
                },
                {"type": "flag", "name": "merge_rollout"},
            ]),
            "anchors must reach the outbox payload in order and unmodified"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_batches_pending_records_with_bearer() {
        let dir = tempdir().unwrap();
        let (embed_base, embed_seen) = spawn_fake_embedder().await;
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        sync.enqueue(record("r1", "content one"));
        sync.enqueue(record("r2", "content two"));
        sync.enqueue(record("r3", "content three"));

        let (base, rec) = spawn_fake_web(None).await;
        let client = reqwest::Client::new();
        let outcome = sync.flush_once(&client, &base).await;
        assert_eq!(outcome, FlushOutcome::Flushed(3));

        let g = rec.lock().await;
        assert_eq!(g.bodies.len(), 1, "one batched POST, not one per record");
        let records = g.bodies[0]["records"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        for r in records {
            assert!(r["title"].is_string());
            assert!(r["content"].is_string());
            assert_eq!(r["kind"], "episode");
            assert!(r["importance"].is_number());
            assert_eq!(r["source"]["task_run_id"], "tr-1");
            assert_eq!(
                r["anchors"],
                json!([]),
                "anchors must survive the drain — the column is NOT NULL"
            );
        }
        assert_eq!(g.auth_headers[0].as_deref(), Some("Bearer test.jwt"));
        drop(g);

        assert!(
            outbox.pending().unwrap().is_empty(),
            "flushed records must be acked"
        );

        // The embed ran locally, over each record's content, in one batch.
        assert_eq!(
            embed_seen.lock().await.as_slice(),
            ["content one", "content two", "content three"],
            "the drain must embed each record's content locally before the POST"
        );
    }

    /// Phase 1 of `2026-07-13-runner-paid-embedding`: the runner ships the
    /// VECTOR, not just the text — and each record gets ITS OWN vector, in
    /// order (the fake embedder returns index-keyed vectors so a zip/order bug
    /// cannot pass).
    #[tokio::test(flavor = "multi_thread")]
    async fn flush_attaches_a_per_record_embedding_and_model_tag() {
        let dir = tempdir().unwrap();
        let (embed_base, _seen) = spawn_fake_embedder().await;
        let (sync, _outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        sync.enqueue(record("r1", "content one"));
        sync.enqueue(record("r2", "content two"));

        let (base, rec) = spawn_fake_web(None).await;
        assert_eq!(
            sync.flush_once(&reqwest::Client::new(), &base).await,
            FlushOutcome::Flushed(2)
        );

        let g = rec.lock().await;
        let records = g.bodies[0]["records"].as_array().unwrap();
        for (i, r) in records.iter().enumerate() {
            let embedding = r["embedding"]
                .as_array()
                .unwrap_or_else(|| panic!("record {i} has no embedding: {r}"));
            assert_eq!(embedding.len(), 384, "MiniLM-L6-v2 is 384-dimensional");
            assert_eq!(
                embedding[0].as_f64().unwrap(),
                i as f64,
                "record {i} must carry ITS OWN vector — vectors must not be \
                 transposed or reused across records"
            );
            assert_eq!(
                r["embedding_model"], EMBEDDING_MODEL_TAG,
                "every shipped vector must name the space that produced it"
            );
        }
    }

    /// A local embedder outage must NEVER block a write. `embedding` is
    /// nullable: the backend stores a NULL-embedding row (immediately
    /// FTS-retrievable) and enqueues it for later vectorization. Deferring the
    /// write instead would turn a soft degradation into data loss — on a
    /// machine whose embedding server is broken, tenant memory would queue
    /// locally forever and never federate.
    #[tokio::test(flavor = "multi_thread")]
    async fn flush_with_embedder_down_ships_records_unembedded() {
        let dir = tempdir().unwrap();
        // Nothing listens on port 1 — connection refused for both the batch
        // route and `compute_batch_embeddings`' per-text fallback.
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), "http://127.0.0.1:1");
        sync.enqueue(record("r1", "content one"));

        let (base, rec) = spawn_fake_web(None).await;
        let outcome = sync.flush_once(&reqwest::Client::new(), &base).await;

        assert_eq!(
            outcome,
            FlushOutcome::Flushed(1),
            "a local embedder outage must not hold the write hostage"
        );
        assert!(
            outbox.pending().unwrap().is_empty(),
            "the record shipped, so it must be acked"
        );

        let g = rec.lock().await;
        let records = g.bodies[0]["records"].as_array().unwrap();
        assert_eq!(records.len(), 1, "the record must still egress");
        assert_eq!(records[0]["content"], "content one");
        assert!(
            records[0].get("embedding").is_none(),
            "no embedding ⇒ OMIT the field so the backend writes NULL and \
             enqueues it; never invent a placeholder vector"
        );
        assert!(
            records[0].get("embedding_model").is_none(),
            "an absent vector must not be tagged with a space that never produced it"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_respects_quota_429_without_dropping() {
        let dir = tempdir().unwrap();
        let (embed_base, _seen) = spawn_fake_embedder().await;
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        sync.enqueue(record("r1", "content one"));
        sync.enqueue(record("r2", "content two"));

        let (base, _rec) = spawn_fake_web(Some(429)).await;
        let client = reqwest::Client::new();
        let outcome = sync.flush_once(&client, &base).await;
        assert_eq!(outcome, FlushOutcome::RateLimited);
        assert_eq!(
            outbox.pending().unwrap().len(),
            2,
            "429 must keep the batch pending (retry later), never drop"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_drops_permanently_rejected_batches() {
        let dir = tempdir().unwrap();
        let (embed_base, _seen) = spawn_fake_embedder().await;
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        sync.enqueue(record("r1", "content one"));

        let (base, _rec) = spawn_fake_web(Some(422)).await;
        let client = reqwest::Client::new();
        let outcome = sync.flush_once(&client, &base).await;
        assert_eq!(outcome, FlushOutcome::Dropped(1));
        assert!(
            outbox.pending().unwrap().is_empty(),
            "validation-rejected batch is ack-dropped so the queue moves"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_without_bearer_keeps_records_pending() {
        let dir = tempdir().unwrap();
        // Unroutable embedder: if the bearer gate did NOT precede the embed,
        // this would surface as Retry("embedding: …") instead of NoAuth.
        let (sync, outbox) = make_flushing_sync(dir.path(), None, "http://127.0.0.1:1");
        sync.enqueue(record("r1", "content one"));

        let (base, rec) = spawn_fake_web(None).await;
        let client = reqwest::Client::new();
        let outcome = sync.flush_once(&client, &base).await;
        assert_eq!(
            outcome,
            FlushOutcome::NoAuth,
            "the bearer gate must short-circuit BEFORE any embedding compute"
        );
        assert_eq!(outbox.pending().unwrap().len(), 1);
        assert!(
            rec.lock().await.bodies.is_empty(),
            "no request may go out without a device JWT"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_transport_error_keeps_records_pending() {
        let dir = tempdir().unwrap();
        // Embedder up, web backend unreachable — isolates the POST transport
        // failure from the embed failure.
        let (embed_base, _seen) = spawn_fake_embedder().await;
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        sync.enqueue(record("r1", "content one"));

        // Unroutable port — connection refused.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let outcome = sync.flush_once(&client, "http://127.0.0.1:1").await;
        assert!(matches!(outcome, FlushOutcome::Retry(_)), "got {outcome:?}");
        assert_eq!(
            outbox.pending().unwrap().len(),
            1,
            "offline keeps the queue"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_caps_batches_at_max_batch() {
        let dir = tempdir().unwrap();
        let (embed_base, _seen) = spawn_fake_embedder().await;
        let (sync, outbox) = make_flushing_sync(dir.path(), Some("test.jwt"), &embed_base);
        for i in 0..(MAX_BATCH + 5) {
            sync.enqueue(record(&format!("r{i}"), &format!("content {i}")));
        }

        let (base, rec) = spawn_fake_web(None).await;
        let client = reqwest::Client::new();
        assert_eq!(
            sync.flush_once(&client, &base).await,
            FlushOutcome::Flushed(MAX_BATCH)
        );
        assert_eq!(
            outbox.pending().unwrap().len(),
            5,
            "tail stays for next pass"
        );
        assert_eq!(
            sync.flush_once(&client, &base).await,
            FlushOutcome::Flushed(5)
        );
        let g = rec.lock().await;
        assert_eq!(g.bodies[0]["records"].as_array().unwrap().len(), MAX_BATCH);
        assert_eq!(g.bodies[1]["records"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn truncate_utf8_respects_char_boundaries() {
        let s = "aé漢字";
        for max in 0..=s.len() {
            let t = truncate_utf8(s, max);
            assert!(t.len() <= max);
            assert!(s.starts_with(t));
        }
    }
}
