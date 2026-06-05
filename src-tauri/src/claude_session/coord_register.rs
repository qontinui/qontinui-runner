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
