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
    /// kept pending — retry after backoff.
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
            warned_no_auth: std::sync::Once::new(),
        }
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

        let records: Vec<&JsonValue> = batch.iter().map(|r| &r.payload).collect();
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

/// Resolve the web backend base URL the memory API lives behind. Reuses the
/// shared resolver (`QONTINUI_WEB_BASE` env → web base derived from the
/// active profile's `coord_url`) that pairing + env-agent enroll already
/// use. `None` when neither is available (unconfigured dev box).
pub(crate) fn resolve_web_base() -> Option<String> {
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

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_batches_pending_records_with_bearer() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
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
        }
        assert_eq!(g.auth_headers[0].as_deref(), Some("Bearer test.jwt"));
        drop(g);

        assert!(
            outbox.pending().unwrap().is_empty(),
            "flushed records must be acked"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_respects_quota_429_without_dropping() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
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
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
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
        let (sync, outbox) = make_sync(dir.path(), true, None);
        sync.enqueue(record("r1", "content one"));

        let (base, rec) = spawn_fake_web(None).await;
        let client = reqwest::Client::new();
        let outcome = sync.flush_once(&client, &base).await;
        assert_eq!(outcome, FlushOutcome::NoAuth);
        assert_eq!(outbox.pending().unwrap().len(), 1);
        assert!(
            rec.lock().await.bodies.is_empty(),
            "no request may go out without a device JWT"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flush_transport_error_keeps_records_pending() {
        let dir = tempdir().unwrap();
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
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
        let (sync, outbox) = make_sync(dir.path(), true, Some("test.jwt"));
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
