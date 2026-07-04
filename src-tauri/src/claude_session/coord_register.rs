//! Session-automation Phase 0 — register authenticated `ClaudeSession`s into
//! `coord.sessions` so they become visible, addressable, and correctly stale to
//! coord.
//!
//! Plan: `D:/qontinui-root/qontinui-dev-notes/plans/
//! 2026-06-04-session-automation-injection-engine-design.md` §0/F1 +
//! `2026-06-04-session-automation-phase0-checklist.md` §3 (R1–R6).
//!
//! ## Why a dedicated registrar (not `SessionRegistry::register_external`)
//!
//! The operator's authenticated AI sessions live in
//! [`crate::claude_session::manager::SessionManager`], keyed by `task_run_id`
//! (UUIDv4) — **not** in [`crate::session::SessionRegistry`] (keyed by coord
//! UUIDv7). Two consequences drive this module's shape:
//!
//! 1. **Staleness must reflect operator inactivity, not process liveness
//!    (P0.2 / R3).** `SessionRegistry`'s heartbeat loop
//!    (`coord_sync::run_heartbeat_loop`) PATCHes coord `{heartbeat:true}` for
//!    **every** active registry session every ~15s, unconditionally — a session
//!    mirrored through it would refresh `last_heartbeat_at=now()` forever and
//!    **never** age to `stale` (the 600s `session_staleness_watcher` trigger
//!    would never fire). By registering AI sessions **directly via the outbox**
//!    (bypassing `SessionRegistry`), the only heartbeats they ever get are the
//!    ones this module emits **on operator interaction** (see
//!    [`AiCoordRegistrar::heartbeat_on_interaction`], called from
//!    `send_user_message`). An idle session therefore ages to `stale`, which is
//!    the entire point of Phase 0.
//!
//! 2. **`register_external` has no usable inject transport (P0.4).** It backs
//!    the mirror with the no-op `ExternalTransport` — `write_input` does
//!    nothing. Inject (Phase 1) must route to the live `ClaudeSession` via
//!    `SessionManager.get(task_run_id)`. So coord stores the durable
//!    `session_id ↔ task_run_id` mapping (the new `coord.sessions.task_run_id`
//!    column) and this registrar keeps the runner-local hot-path index (R4).
//!
//! ## Wire path
//!
//! This module writes rows to the **same** [`OutboxWriter`] the `CoordSync`
//! drain loop reads, so registration reuses the entire existing drain → auth →
//! retry → 409-idempotency machinery with no new HTTP code:
//!
//! - **R1/R2 register** → `Started` outbox row (`POST /sessions`) carrying
//!   `session_kind="agentic"`, `task_run_id`, and a nil `tenant_id` (coord
//!   resolves the real tenant from the device, exactly like every other
//!   runner-originated session).
//! - **R3 heartbeat** → `Heartbeat` outbox row (`PATCH {heartbeat:true}`) on
//!   operator interaction only.
//! - **R5 close** → `Closed` outbox row (`DELETE /sessions/:id`) + index evict.
//!
//! ## Gating (P0.3)
//!
//! Registration is gated on `QONTINUI_SESSION_AUTOMATION_REGISTER` (default
//! **ON**) — **independent** of the dormant per-tenant
//! `session_coordination_enabled` dual-write burn-in. Coupling the automation
//! foundation to that unrelated rollout knob would make Phase 0 inert in prod
//! (the flag is dormant). Setting the env to `0`/`false`/`off` disables
//! registration cleanly (a kill switch); the AI session itself is never
//! affected — every coord write here is best-effort and swallows errors.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::session::local_store::OutboxWriter;
use crate::session::SessionEventKind;

/// Default-ON env gate for AI-session coord registration (P0.3). Any of
/// `0` / `false` / `off` (case-insensitive) disables it; anything else
/// (including unset) leaves it ON.
fn registration_enabled() -> bool {
    match std::env::var("QONTINUI_SESSION_AUTOMATION_REGISTER") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off"
        ),
        Err(_) => true,
    }
}

/// Default-ON gate for streaming interactive-session activity to coord's
/// `agent_logs` ingest (Phase 1c). The feature is enabled by default — the
/// rollout burn-in passed and the Phase-2 `session_started` flood fix
/// (runner#637) landed — so deployed AND dev runners emit with no
/// per-environment config (no dev-start.ps1 flag, no machine env var). Set
/// `QONTINUI_AGENT_LOGS_FROM_SESSIONS` to an explicit falsy value — `0` /
/// `false` / `off` / `no` (case-insensitive) — as an ops kill-switch to turn
/// it off; unset (the production default) and any other value leave it ON.
pub fn agent_logs_from_sessions_enabled() -> bool {
    !matches!(
        std::env::var("QONTINUI_AGENT_LOGS_FROM_SESSIONS")
            .map(|v| v.trim().to_ascii_lowercase()),
        Ok(ref v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

/// Registers authenticated `ClaudeSession`s into `coord.sessions` and owns the
/// runner-local `coord session_id (UUIDv7) ↔ task_run_id (UUIDv4)` index (R4).
///
/// Managed as Tauri state; cheap to clone (everything `Arc`/shared). Holds the
/// same [`OutboxWriter`] the `CoordSync` drain loop drains, so a write here is
/// pushed to coord by the existing loop.
#[derive(Clone)]
pub struct AiCoordRegistrar {
    inner: Arc<Inner>,
}

struct Inner {
    outbox: Arc<OutboxWriter>,
    machine_id: Uuid,
    /// R4 index — coord `session_id` (UUIDv7) → runner `task_run_id` (UUIDv4),
    /// plus the reverse map so close-by-`task_run_id` can evict without a scan.
    /// `forward` is the inject resolver Phase 1 consumes; `reverse` is the
    /// lifecycle/heartbeat path keyed by what the AI-session commands already
    /// hold (`task_run_id`).
    forward: Mutex<HashMap<Uuid, String>>,
    reverse: Mutex<HashMap<String, Uuid>>,
}

impl AiCoordRegistrar {
    /// Construct from the shared session outbox + this device's `machine_id`.
    /// The outbox MUST be the same `Arc` the `CoordSync` drain loop reads.
    pub fn new(outbox: Arc<OutboxWriter>, machine_id: Uuid) -> Self {
        Self {
            inner: Arc::new(Inner {
                outbox,
                machine_id,
                forward: Mutex::new(HashMap::new()),
                reverse: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// R4 — resolve a coord `session_id` to its runner `task_run_id`. The
    /// hot-path resolver Phase 1's inject consumer will call before
    /// `SessionManager.get(task_run_id)`.
    pub fn task_run_id_for(&self, session_id: &Uuid) -> Option<String> {
        self.inner
            .forward
            .lock()
            .ok()
            .and_then(|g| g.get(session_id).cloned())
    }

    /// R4 — resolve a runner `task_run_id` to its coord `session_id` (the
    /// durable handle). Useful for the inject-audit / observability path.
    #[allow(dead_code)]
    pub fn session_id_for(&self, task_run_id: &str) -> Option<Uuid> {
        self.inner
            .reverse
            .lock()
            .ok()
            .and_then(|g| g.get(task_run_id).copied())
    }

    /// R1/R2 — register (or idempotently re-register) an authenticated AI
    /// session with coord. Mints a fresh coord `session_id` (UUIDv7), writes a
    /// `Started` outbox row carrying `task_run_id`, and records the R4 index.
    ///
    /// **R6 idempotency:** a re-register for a `task_run_id` already in the
    /// index is a no-op (returns the existing coord id) — a reconnect/restart
    /// never writes a duplicate `Started` row. (Coord additionally treats a
    /// 409 on `POST /sessions` as success in the drain loop.)
    ///
    /// Best-effort: a disabled gate or an outbox write error returns `None`
    /// and never disturbs the live AI session. `purpose` surfaces in the
    /// dashboard Live Sessions panel; `repo` is the optional session repo.
    pub fn register_session(
        &self,
        task_run_id: &str,
        purpose: &str,
        repo: Option<String>,
    ) -> Option<Uuid> {
        if !registration_enabled() {
            debug!(
                "ai_coord_register: disabled via QONTINUI_SESSION_AUTOMATION_REGISTER — skipping {}",
                task_run_id
            );
            return None;
        }

        // R6 — already registered? Return the existing coord id, write nothing.
        if let Some(existing) = self.session_id_for(task_run_id) {
            debug!(
                "ai_coord_register: {} already registered as coord session {} — no-op",
                task_run_id, existing
            );
            return Some(existing);
        }

        let session_id = crate::session::uuid_v7();
        let now = chrono::Utc::now();
        // Intent shape mirrors `coord.sessions.intent` (JSONB) — purpose is
        // required (min 3 chars coord-side); fall back to a stable default so a
        // blank chat name never trips coord's intent validation.
        let purpose = {
            let t = purpose.trim();
            if t.len() >= 3 {
                t.to_string()
            } else {
                "AI session".to_string()
            }
        };
        let mut intent = json!({ "purpose": purpose });
        if let Some(r) = repo.as_ref() {
            intent["repo"] = json!(r);
        }

        // The `Started` payload is the create-body shape `rebuild_create_body`
        // relabels for `POST /sessions`. `tenant_id` is omitted (nil) so coord
        // resolves the real tenant from the device registration — the same path
        // every runner-originated session uses.
        let payload = json!({
            "id": session_id,
            "kind": SessionKind::Agentic.as_str(),
            "intent": intent,
            "state": "active",
            "started_at": now,
            "task_run_id": task_run_id,
        });

        if let Err(e) = self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::Started,
            payload,
        ) {
            warn!(
                "ai_coord_register: outbox Started write failed for {} (best-effort): {}",
                task_run_id, e
            );
            return None;
        }

        // Record the R4 index only after the durable outbox write succeeds.
        if let (Ok(mut fwd), Ok(mut rev)) = (self.inner.forward.lock(), self.inner.reverse.lock()) {
            fwd.insert(session_id, task_run_id.to_string());
            rev.insert(task_run_id.to_string(), session_id);
        }

        info!(
            "ai_coord_register: registered AI session {} as coord session {} (kind=agentic)",
            task_run_id, session_id
        );
        Some(session_id)
    }

    /// Commit ↔ session lineage push-report (plan
    /// `2026-06-07-coord-commit-session-lineage.md`, Population path 2). Enqueue
    /// a `commit_report` outbox row carrying `{repo, branch, shas}`. The drain
    /// loop POSTs it to `POST /coord/commits/report`; coord resolves the
    /// session server-side from `(repo, branch)`, so the body carries NO session
    /// id.
    ///
    /// The outbox keys its monotonic `seq` on `(machine_id, session_id)`, but
    /// commit reports have no real session — we mint a **deterministic** UUIDv5
    /// from `(repo, branch)` so all reports for one branch share a seq lane
    /// (stable ordering, idempotent replay) without colliding across branches.
    /// Coord never reads this id.
    ///
    /// Best-effort and gated on `QONTINUI_COMMIT_LINEAGE_REPORT` (default ON);
    /// a disabled gate, empty `shas`, or an outbox write error is a silent
    /// no-op that never disturbs the live session.
    pub fn report_commits(&self, repo: &str, branch: &str, shas: Vec<String>) {
        if !crate::terminal::commit_report::report_enabled() {
            return;
        }
        let shas: Vec<String> = shas.into_iter().filter(|s| !s.trim().is_empty()).collect();
        if shas.is_empty() {
            return;
        }

        // Deterministic per-(repo, branch) synthetic session id for the outbox
        // seq lane. URL namespace + "repo/branch" key.
        let session_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("commit-report:{repo}:{branch}").as_bytes(),
        );
        let payload = json!({
            "repo": repo,
            "branch": branch,
            "shas": shas,
        });

        match self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::CommitReport,
            payload,
        ) {
            Ok(_) => info!(
                "ai_coord_register: enqueued commit report for {}@{} ({} sha(s))",
                repo,
                branch,
                shas.len()
            ),
            Err(e) => warn!(
                "ai_coord_register: outbox CommitReport write failed for {}@{} (best-effort): {}",
                repo, branch, e
            ),
        }
    }

    /// R3 — emit a coord heartbeat for the AI session backing `task_run_id`,
    /// driven by **operator interaction** (called from `send_user_message`).
    /// This is the ONLY heartbeat these sessions ever get, so an idle session
    /// ages to `stale` at coord's threshold. No-op (silently) if the session
    /// was never registered or the gate is off.
    pub fn heartbeat_on_interaction(&self, task_run_id: &str) {
        let Some(session_id) = self.session_id_for(task_run_id) else {
            return;
        };
        let payload = json!({ "id": session_id, "at": chrono::Utc::now() });
        if let Err(e) = self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::Heartbeat,
            payload,
        ) {
            warn!(
                "ai_coord_register: outbox Heartbeat write failed for {} (best-effort): {}",
                task_run_id, e
            );
        } else {
            debug!(
                "ai_coord_register: interaction heartbeat for {} (coord {})",
                task_run_id, session_id
            );
        }
    }

    /// Advance this session's **work-progress** clock
    /// (`coord.sessions.last_progress_at`) on operator interaction — a sibling
    /// of [`Self::heartbeat_on_interaction`], called from the same
    /// `send_user_message` work-activity boundary. Plan
    /// `2026-06-25-session-progress-reporting-and-agent-session-linkage.md`.
    ///
    /// This is the missing *producer* the prior plan's session-stall watcher
    /// was starved of: it advances `last_progress_at` on the **work axis**,
    /// orthogonal to the liveness `Heartbeat` (a session can hold a claim yet
    /// stop advancing — the watcher flips such a session to `stalled`). The
    /// `Progress` outbox row drains to `PATCH /sessions/:id {progress:{…}}`;
    /// coord stamps `last_progress_at = now()` (we send no explicit timestamp).
    ///
    /// No-op (silently) if the session was never registered — which is also how
    /// the `QONTINUI_SESSION_AUTOMATION_REGISTER` kill switch disables it: a
    /// disabled session has no R4 index entry, so this resolves to `None`.
    /// Best-effort throughout — a write failure never disturbs the live session.
    pub fn progress_on_interaction(&self, task_run_id: &str) {
        let Some(session_id) = self.session_id_for(task_run_id) else {
            return;
        };
        // Minimal body: `session_status="working"`. Coord stamps
        // `last_progress_at=now()` on receipt (no explicit `last_progress_at`
        // or `progress_detail` sent — the interaction itself is the signal).
        let payload = json!({ "id": session_id, "session_status": "working" });
        if let Err(e) = self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::Progress,
            payload,
        ) {
            warn!(
                "ai_coord_register: outbox Progress write failed for {} (best-effort): {}",
                task_run_id, e
            );
        } else {
            debug!(
                "ai_coord_register: interaction progress for {} (coord {})",
                task_run_id, session_id
            );
        }
    }

    /// R5 — on AI-session end, emit a `Closed` outbox row (`DELETE
    /// /sessions/:id`) and evict the R4 index entry so coord.sessions doesn't
    /// leak a ghost row and the resolver doesn't keep a dangling mapping.
    /// No-op if the session wasn't registered.
    pub fn close_session(&self, task_run_id: &str) {
        let session_id = {
            // Evict reverse first, capturing the coord id.
            let Some(id) = self
                .inner
                .reverse
                .lock()
                .ok()
                .and_then(|mut g| g.remove(task_run_id))
            else {
                return;
            };
            if let Ok(mut fwd) = self.inner.forward.lock() {
                fwd.remove(&id);
            }
            id
        };

        // A `Closed` row carries no body — the drain loop maps it to
        // `DELETE /sessions/:id`. Best-effort; a missing coord row DELETEs as
        // idempotent success.
        if let Err(e) = self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::Closed,
            json!({ "id": session_id }),
        ) {
            warn!(
                "ai_coord_register: outbox Closed write failed for {} (best-effort): {}",
                task_run_id, e
            );
        } else {
            info!(
                "ai_coord_register: closed AI session {} (coord {})",
                task_run_id, session_id
            );
        }
    }
}

use crate::session::SessionKind;

// ============================================================================
// Interactive-agent log emitter (Phase 1b/1c)
// ============================================================================
//
// Interactive / runner-managed Claude sessions register into `coord.sessions`
// (Live Sessions panel) but emit no `coord.agent_logs` rows, so they are
// invisible on the `/admin/coord/agents` dashboard. This emitter streams a
// session's activity to coord's existing log ingest `POST /agents/:id/log`,
// tagging each entry with the runner's `device_id` so coord's Phase-1a
// fallback resolves the tenant from `coord.devices`.
//
// Wire shape is coord's [`LogEntry`] (`qontinui-coord/src/agent_logs.rs`):
// `level` + `event` are REQUIRED and non-empty (coord 400s otherwise);
// `payload` / `agent_session_id` / `device_id` / `occurred_at` are optional.
// `agent_id` goes in the URL path, never the body.
//
// Identity: the coord session UUIDv7 is BOTH the URL `agent_id` AND the body
// `agent_session_id` (one identity shared with Live Sessions). The runner's
// `device_id` is read once from `~/.qontinui/machine.json`.
//
// Batching: a `std::sync::mpsc` channel feeds a background flush thread that
// drains the queue into a single `Vec<LogEntry>` and POSTs the whole array in
// one request (≤ `MAX_AGENT_LOG_BATCH`, mirroring coord's `MAX_BATCH_SIZE`).
// A 5s periodic flush + try-immediate-then-requeue-on-failure + a final flush
// on close mirror `agent_runtime::pump_subprocess`'s shape, but batched.
//
// Everything here is gated on [`agent_logs_from_sessions_enabled`] (default
// OFF) at the construction site, so an OFF gate means no emitter is ever built
// and zero coord traffic is added.

/// Max entries per `POST /agents/:id/log` batch. Mirrors coord's
/// `agent_logs::MAX_BATCH_SIZE`; coord 400s a larger batch.
const MAX_AGENT_LOG_BATCH: usize = 500;

/// Periodic flush cadence for the background drain thread.
const AGENT_LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Upper bound on buffered entries when coord is unreachable. Older entries
/// drop FIFO so an offline coord can't grow the queue without bound. Generous
/// (10×batch) since each entry is small.
const AGENT_LOG_QUEUE_CAP: usize = MAX_AGENT_LOG_BATCH * 10;

/// One `coord.agent_logs` row in coord's wire shape. `level` + `event` MUST be
/// non-empty or coord rejects the whole batch with 400.
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub level: String,
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<Uuid>,
    /// Phase 8b (plan 2026-07-02-session-scoped-multi-tenant-device-binding,
    /// Phase 8 item 7 / D2 site 14): EXPLICIT tenant attribution from the
    /// owning session's binding — these emitters serve runner-managed
    /// operator sessions, whose binding is the device DEFAULT. Coord's
    /// ingest fallback chain prefers it once Phase 5 deploys; today's coord
    /// ignores the extra field. Omitted when unresolvable (coord then
    /// resolves agent→worktree→device-side, the pre-8b behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Messages the session pushes to the emitter's background drain thread.
enum EmitMsg {
    /// A fully-formed log entry to enqueue.
    Entry(LogEntry),
    /// Close signal — drain the queue one final time, then stop.
    Close,
}

/// Cheap-to-clone handle the session threads push log entries into. The actual
/// HTTP work happens on a single background drain thread; pushing is a
/// non-blocking channel send. Dropping the last handle (or calling
/// [`AgentLogEmitter::close`]) ends the drain thread after a final flush.
#[derive(Clone)]
pub struct AgentLogEmitter {
    tx: std::sync::mpsc::Sender<EmitMsg>,
    /// The coord session id — both the URL `agent_id` and the body
    /// `agent_session_id`. Carried so emit helpers can stamp it without the
    /// caller re-threading it.
    agent_id: Uuid,
    device_id: Option<Uuid>,
    /// Phase 8b item 7 — the owning session's tenant binding, resolved once
    /// at emitter start (device DEFAULT for these runner-managed sessions)
    /// and stamped on every entry.
    tenant_id: Option<Uuid>,
}

impl AgentLogEmitter {
    /// Build an emitter for the coord session `agent_id`, reading `device_id`
    /// once from `~/.qontinui/machine.json`. Returns `None` (no emitter, no
    /// thread) when the gate is OFF or `device_id` is unreadable — both are
    /// fail-safe no-ops: coord's Phase-1a tenant fallback needs the
    /// `device_id`, so emitting without it is pointless.
    pub fn start(agent_id: Uuid) -> Option<Self> {
        if !agent_logs_from_sessions_enabled() {
            return None;
        }
        let device_id = match read_device_id_uuid() {
            Some(d) => d,
            None => {
                debug!(
                    "agent_log_emitter: no readable device_id for agent {} — skipping emission",
                    agent_id
                );
                return None;
            }
        };

        let (tx, rx) = std::sync::mpsc::channel::<EmitMsg>();
        std::thread::Builder::new()
            .name(format!("agent-log-emit-{agent_id}"))
            .spawn(move || drain_loop(agent_id, rx))
            .map_err(|e| warn!("agent_log_emitter: failed to spawn drain thread: {e}"))
            .ok()?;

        info!(
            "agent_log_emitter: streaming session {} to coord agent_logs",
            agent_id
        );
        Some(Self {
            tx,
            agent_id,
            device_id: Some(device_id),
            // Phase 8b item 7 — resolve the owning session's binding once
            // (device DEFAULT: these are runner-managed operator sessions;
            // coord-spawned agent sessions don't stream through this
            // emitter). Best-effort: `None` omits the field on the wire.
            tenant_id: crate::fleet::resolve_tenant_id(),
        })
    }

    /// Build a `LogEntry` stamped with this emitter's identity (agent session
    /// id + device id + owning tenant), letting coord stamp `occurred_at`.
    fn entry(&self, level: &str, event: &str, payload: Option<serde_json::Value>) -> LogEntry {
        LogEntry {
            level: level.to_string(),
            event: event.to_string(),
            payload,
            agent_session_id: Some(self.agent_id),
            device_id: self.device_id,
            tenant_id: self.tenant_id,
            occurred_at: None,
        }
    }

    /// Emit an arbitrary `LogEntry` with the given `level` / `event` /
    /// `payload`, stamped with this emitter's identity. Used by callers that
    /// produce non-`stdout` events (e.g. the transcript watcher's `assistant` /
    /// `tool_use` lines). Empty `event` is dropped — coord 400s an empty event,
    /// so swallowing it here keeps the batch valid.
    pub fn emit(&self, level: &str, event: &str, payload: Option<serde_json::Value>) {
        if event.trim().is_empty() {
            return;
        }
        let _ = self
            .tx
            .send(EmitMsg::Entry(self.entry(level, event, payload)));
    }

    /// `session_started` milestone — one line at session start.
    pub fn started(&self, purpose: &str) {
        let _ = self.tx.send(EmitMsg::Entry(self.entry(
            "info",
            "session_started",
            Some(json!({ "purpose": purpose })),
        )));
    }

    /// Per-stdout-line activity. Empty lines are dropped (no signal, and an
    /// empty `event` would never happen here but an empty `text` is just
    /// noise). `event` is the constant `"stdout"`; the raw line rides in
    /// `payload.text`.
    pub fn stream_line(&self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let _ = self.tx.send(EmitMsg::Entry(self.entry(
            "info",
            "stdout",
            Some(json!({ "text": line })),
        )));
    }

    /// Terminal `session_closed` milestone, then signal the drain thread to do
    /// its final flush and stop. Idempotent-safe: a second close just enqueues
    /// another (harmless) terminal line if the channel is still open.
    pub fn close(&self) {
        let _ = self
            .tx
            .send(EmitMsg::Entry(self.entry("info", "session_closed", None)));
        let _ = self.tx.send(EmitMsg::Close);
    }
}

/// Read `device_id` from `~/.qontinui/machine.json` and parse it as a `Uuid`.
/// Reuses the canonical disk reader in [`crate::pair`]. Any failure (missing
/// file, unparseable, non-UUID) yields `None` — the emitter then skips.
fn read_device_id_uuid() -> Option<Uuid> {
    // `pair` lives in the `qontinui_runner_lib` crate (this module compiles into
    // both the lib and the bin target, so the bin can't reach it via `crate::`).
    let raw = qontinui_runner_lib::pair::read_device_id_from_disk().ok()?;
    Uuid::parse_str(raw.trim()).ok()
}

/// Background drain loop: owns the reqwest client + the FIFO queue. Wakes on a
/// channel recv with a `AGENT_LOG_FLUSH_INTERVAL` timeout so an idle session
/// still flushes within ~5s. Drains the queue into one `Vec<LogEntry>`
/// (≤ batch cap) and POSTs the whole array; on POST failure the un-sent tail
/// is requeued (capped FIFO) for the next tick. Ends on `Close` or when all
/// `AgentLogEmitter` handles drop (channel disconnect), with a final flush.
fn drain_loop(agent_id: Uuid, rx: std::sync::mpsc::Receiver<EmitMsg>) {
    let mut queue: std::collections::VecDeque<LogEntry> = std::collections::VecDeque::new();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok();

    loop {
        match rx.recv_timeout(AGENT_LOG_FLUSH_INTERVAL) {
            Ok(EmitMsg::Entry(e)) => {
                if queue.len() >= AGENT_LOG_QUEUE_CAP {
                    queue.pop_front();
                }
                queue.push_back(e);
                // Coalesce a burst: pull any other immediately-ready entries
                // without blocking, so a flurry of stdout lines POSTs as ONE
                // batch rather than one request per line.
                while let Ok(EmitMsg::Entry(e)) = rx.try_recv() {
                    if queue.len() >= AGENT_LOG_QUEUE_CAP {
                        queue.pop_front();
                    }
                    queue.push_back(e);
                }
                if queue.len() >= MAX_AGENT_LOG_BATCH {
                    flush_batch(client.as_ref(), agent_id, &mut queue);
                }
            }
            Ok(EmitMsg::Close) => {
                flush_batch(client.as_ref(), agent_id, &mut queue);
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                flush_batch(client.as_ref(), agent_id, &mut queue);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                flush_batch(client.as_ref(), agent_id, &mut queue);
                break;
            }
        }
    }
    debug!("agent_log_emitter: drain thread for {} stopped", agent_id);
}

/// Drain up to `MAX_AGENT_LOG_BATCH` entries from the front of `queue` and POST
/// them as one array. On failure (no client, no coord base, HTTP error) the
/// drained slice is pushed back to the FRONT in order so nothing is lost and
/// the next tick retries — bounded by the caller's queue cap. Best-effort:
/// never panics, never blocks the session.
fn flush_batch(
    client: Option<&reqwest::blocking::Client>,
    agent_id: Uuid,
    queue: &mut std::collections::VecDeque<LogEntry>,
) {
    if queue.is_empty() {
        return;
    }
    let Some(client) = client else {
        return; // client build failed earlier — keep buffering (capped).
    };
    let Some(base) = coord_http_base() else {
        return; // coord not configured — keep buffering (capped).
    };

    let take = queue.len().min(MAX_AGENT_LOG_BATCH);
    let batch: Vec<LogEntry> = queue.drain(..take).collect();

    // Best-effort bearer auth (mirrors the federation report path). The ingest
    // route resolves the tenant from the body `device_id`, so a missing token
    // is non-fatal — we just omit the header.
    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .filter(|t| !t.is_empty());

    let url = format!("{base}/agents/{agent_id}/log");
    let mut req = client.post(&url).json(&batch);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }

    match req.send() {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "agent_log_emitter: flushed {} entries for {}",
                batch.len(),
                agent_id
            );
        }
        Ok(resp) => {
            warn!(
                "agent_log_emitter: POST /agents/{}/log returned {} — requeueing {} entries",
                agent_id,
                resp.status(),
                batch.len()
            );
            requeue_front(queue, batch);
        }
        Err(e) => {
            debug!(
                "agent_log_emitter: POST /agents/{}/log failed ({e}) — requeueing {} entries",
                agent_id,
                batch.len()
            );
            requeue_front(queue, batch);
        }
    }
}

/// Push a failed batch back to the FRONT of the queue in original order,
/// trimming the tail to honor `AGENT_LOG_QUEUE_CAP` (drop oldest on overflow).
fn requeue_front(queue: &mut std::collections::VecDeque<LogEntry>, batch: Vec<LogEntry>) {
    for e in batch.into_iter().rev() {
        queue.push_front(e);
    }
    while queue.len() > AGENT_LOG_QUEUE_CAP {
        queue.pop_back();
    }
}

/// Resolve the coord HTTP base (env `COORD_HTTP_URL` → active profile
/// `coord_url`). `None` when nothing is configured — the emitter then keeps
/// buffering (capped) rather than dropping, matching the other resolvers'
/// no-localhost-fallback posture.
fn coord_http_base() -> Option<String> {
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => {
            Some(base.trim_end_matches('/').to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    /// `QONTINUI_SESSION_AUTOMATION_REGISTER` is process-global mutable
    /// state; tests that set/remove it race when the harness runs them on
    /// parallel threads (one test's `remove_var` lands between another's
    /// `set_var("0")` and its assert — seen flaking on windows-latest).
    /// Every env-touching test holds this lock for its full body. Poisoning
    /// is harmless — each test resets the var on entry — so recover with
    /// `into_inner`.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn registrar() -> (AiCoordRegistrar, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let outbox = Arc::new(OutboxWriter::open(dir.path().join("outbox.jsonl")).unwrap());
        (AiCoordRegistrar::new(outbox, Uuid::new_v4()), dir)
    }

    #[test]
    fn register_writes_started_row_and_indexes_both_directions() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        let coord_id = reg.register_session(&trid, "fix the thing", None).unwrap();

        // R4 index resolves both ways.
        assert_eq!(
            reg.task_run_id_for(&coord_id).as_deref(),
            Some(trid.as_str())
        );
        assert_eq!(reg.session_id_for(&trid), Some(coord_id));

        // Exactly one Started row in the outbox, carrying task_run_id + agentic.
        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, SessionEventKind::Started.as_str());
        assert_eq!(pending[0].payload["task_run_id"], json!(trid));
        assert_eq!(pending[0].payload["kind"], json!("agentic"));
        assert_eq!(pending[0].payload["id"], json!(coord_id));
    }

    #[test]
    fn reregister_is_idempotent_no_duplicate_started_row() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        let first = reg.register_session(&trid, "purpose", None).unwrap();
        let second = reg.register_session(&trid, "purpose", None).unwrap();
        assert_eq!(first, second, "re-register returns the same coord id");

        // Still exactly one Started row — R6 idempotency.
        let started = reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .count();
        assert_eq!(started, 1);
    }

    #[test]
    fn heartbeat_only_after_register_and_keyed_by_session() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        // Heartbeat before registration is a no-op (no row).
        reg.heartbeat_on_interaction(&trid);
        assert!(reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .iter()
            .all(|r| r.event_kind != SessionEventKind::Heartbeat.as_str()));

        let coord_id = reg.register_session(&trid, "purpose", None).unwrap();
        reg.heartbeat_on_interaction(&trid);

        let hb: Vec<_> = reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Heartbeat.as_str())
            .collect();
        assert_eq!(hb.len(), 1, "exactly one interaction heartbeat");
        assert_eq!(hb[0].session_id, coord_id);
    }

    #[test]
    fn progress_only_after_register_and_keyed_by_session() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        // Progress before registration is a no-op (no row) — this is also how
        // the kill switch disables it (unregistered → no R4 index entry).
        reg.progress_on_interaction(&trid);
        assert!(reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .iter()
            .all(|r| r.event_kind != SessionEventKind::Progress.as_str()));

        let coord_id = reg.register_session(&trid, "purpose", None).unwrap();
        reg.progress_on_interaction(&trid);

        let prog: Vec<_> = reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Progress.as_str())
            .collect();
        assert_eq!(prog.len(), 1, "exactly one interaction progress row");
        assert_eq!(prog[0].session_id, coord_id);
        // Minimal body advances the work axis; coord stamps last_progress_at.
        assert_eq!(prog[0].payload["session_status"], json!("working"));
    }

    #[test]
    fn progress_disabled_gate_is_noop() {
        let _env = env_lock();
        std::env::set_var("QONTINUI_SESSION_AUTOMATION_REGISTER", "0");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();
        // Disabled → register is a no-op → no index entry → progress no-ops.
        assert!(reg.register_session(&trid, "purpose", None).is_none());
        reg.progress_on_interaction(&trid);
        assert!(reg.inner.outbox.pending().unwrap().is_empty());
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
    }

    #[test]
    fn close_emits_closed_row_and_evicts_index() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        let coord_id = reg.register_session(&trid, "purpose", None).unwrap();
        reg.close_session(&trid);

        // Index evicted both ways.
        assert!(reg.task_run_id_for(&coord_id).is_none());
        assert!(reg.session_id_for(&trid).is_none());

        // A Closed row was written for the coord id.
        let closed =
            reg.inner.outbox.pending().unwrap().into_iter().any(|r| {
                r.event_kind == SessionEventKind::Closed.as_str() && r.session_id == coord_id
            });
        assert!(closed, "a Closed outbox row must be emitted on close");
    }

    #[test]
    fn report_commits_enqueues_commit_report_row() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_COMMIT_LINEAGE_REPORT");
        let (reg, _dir) = registrar();

        reg.report_commits(
            "qontinui/qontinui-runner",
            "feat/x",
            vec!["sha1".into(), "sha2".into()],
        );

        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        let row = &pending[0];
        assert_eq!(row.event_kind, SessionEventKind::CommitReport.as_str());
        assert_eq!(row.payload["repo"], json!("qontinui/qontinui-runner"));
        assert_eq!(row.payload["branch"], json!("feat/x"));
        assert_eq!(row.payload["shas"], json!(["sha1", "sha2"]));
        // No session id is carried in the body — coord resolves it server-side.
        assert!(row.payload.get("agent_session_id").is_none());
    }

    #[test]
    fn report_commits_per_branch_seq_lane_is_deterministic() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_COMMIT_LINEAGE_REPORT");
        let (reg, _dir) = registrar();

        reg.report_commits("o/r", "main", vec!["a".into()]);
        reg.report_commits("o/r", "main", vec!["b".into()]);
        let pending = reg.inner.outbox.pending().unwrap();
        // Both reports for the same (repo, branch) share one seq lane (same
        // synthetic session id), so seqs are 1 then 2.
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].session_id, pending[1].session_id);
        assert_eq!(pending[0].seq, 1);
        assert_eq!(pending[1].seq, 2);
    }

    #[test]
    fn report_commits_skips_empty_shas() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_COMMIT_LINEAGE_REPORT");
        let (reg, _dir) = registrar();
        reg.report_commits("o/r", "main", vec![]);
        reg.report_commits("o/r", "main", vec!["   ".into()]);
        assert!(reg.inner.outbox.pending().unwrap().is_empty());
    }

    #[test]
    fn report_commits_disabled_gate_is_noop() {
        let _env = env_lock();
        std::env::set_var("QONTINUI_COMMIT_LINEAGE_REPORT", "0");
        let (reg, _dir) = registrar();
        reg.report_commits("o/r", "main", vec!["a".into()]);
        assert!(reg.inner.outbox.pending().unwrap().is_empty());
        std::env::remove_var("QONTINUI_COMMIT_LINEAGE_REPORT");
    }

    #[test]
    fn agent_logs_gate_defaults_on_and_only_falsy_disables() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS");
        // Default (unset) is ON — the feature is standard after the rollout
        // burn-in; deployed runners need no per-environment config.
        assert!(agent_logs_from_sessions_enabled());

        // Only an explicit falsy value disables it (the ops kill-switch).
        for off in ["0", "false", "off", "no", "FALSE", "Off", "No"] {
            std::env::set_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS", off);
            assert!(
                !agent_logs_from_sessions_enabled(),
                "value {off:?} must disable the gate (kill-switch)"
            );
        }
        // Unset, truthy, empty, or unrecognized all leave it ON.
        for on in ["1", "true", "on", "yes", "", "garbage"] {
            std::env::set_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS", on);
            assert!(
                agent_logs_from_sessions_enabled(),
                "value {on:?} must leave the gate ON (only explicit falsy disables)"
            );
        }
        std::env::remove_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS");
    }

    #[test]
    fn emitter_off_gate_yields_no_emitter() {
        let _env = env_lock();
        std::env::set_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS", "0");
        assert!(AgentLogEmitter::start(Uuid::new_v4()).is_none());
        std::env::remove_var("QONTINUI_AGENT_LOGS_FROM_SESSIONS");
    }

    #[test]
    fn log_entry_serializes_to_coord_wire_shape() {
        let agent = Uuid::new_v4();
        let device = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let entry = LogEntry {
            level: "info".to_string(),
            event: "stdout".to_string(),
            payload: Some(json!({ "text": "hello" })),
            agent_session_id: Some(agent),
            device_id: Some(device),
            tenant_id: Some(tenant),
            occurred_at: None,
        };
        let v = serde_json::to_value(&entry).unwrap();
        // Required non-empty fields coord validates.
        assert_eq!(v["level"], json!("info"));
        assert_eq!(v["event"], json!("stdout"));
        assert_eq!(v["payload"]["text"], json!("hello"));
        assert_eq!(v["agent_session_id"], json!(agent));
        assert_eq!(v["device_id"], json!(device));
        // Phase 8b item 7 — explicit tenant attribution on the wire.
        assert_eq!(v["tenant_id"], json!(tenant));
        // `occurred_at` omitted (skip_serializing_if None) so coord stamps now().
        assert!(v.get("occurred_at").is_none());
    }

    /// Phase 8b — a None tenant_id is omitted (pre-8b wire shape preserved
    /// for unpaired runners; coord's fallback chain still resolves).
    #[test]
    fn log_entry_omits_none_tenant_id() {
        let entry = LogEntry {
            level: "info".to_string(),
            event: "stdout".to_string(),
            payload: None,
            agent_session_id: None,
            device_id: None,
            tenant_id: None,
            occurred_at: None,
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert!(v.get("tenant_id").is_none());
    }

    #[test]
    fn requeue_front_preserves_order_and_honors_cap() {
        let mk = |n: usize| LogEntry {
            level: "info".to_string(),
            event: format!("e{n}"),
            payload: None,
            agent_session_id: None,
            device_id: None,
            tenant_id: None,
            occurred_at: None,
        };
        let mut q: std::collections::VecDeque<LogEntry> = std::collections::VecDeque::new();
        q.push_back(mk(100)); // an existing tail entry
                              // Requeue a failed batch [1,2,3] — must land at the FRONT in order.
        requeue_front(&mut q, vec![mk(1), mk(2), mk(3)]);
        let events: Vec<String> = q.iter().map(|e| e.event.clone()).collect();
        assert_eq!(events, vec!["e1", "e2", "e3", "e100"]);
    }

    #[test]
    fn disabled_gate_registers_nothing() {
        let _env = env_lock();
        std::env::set_var("QONTINUI_SESSION_AUTOMATION_REGISTER", "0");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        assert!(reg.register_session(&trid, "purpose", None).is_none());
        assert!(reg.inner.outbox.pending().unwrap().is_empty());
        assert!(reg.session_id_for(&trid).is_none());

        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
    }
}
