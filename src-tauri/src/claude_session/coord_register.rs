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
//! - **R1/R2 register** → `Started` outbox row (`POST /sessions`) carrying a
//!   nil `tenant_id` (coord resolves the real tenant from the device, exactly
//!   like every other runner-originated session) plus, per plane:
//!   `session_kind="agentic"` + `task_run_id` for pinned registrar-managed
//!   sessions, or `session_kind="terminal_claude"` + `claude_code_session_id`
//!   for sniffed interactive sessions (fabric Phase 3,
//!   [`AiCoordRegistrar::register_sniffed_session`]).
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
use std::sync::{Arc, Mutex, OnceLock};
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
    /// R4 index — coord `session_id` (UUIDv7) → the session's durable
    /// `claude_session_id` (== `task_run_id` for the pinned plane; the typed
    /// `--resume` id for the sniffed plane), plus the reverse map so
    /// close/evict and caller-self resolution work without a scan. `forward`
    /// is the inject resolver Phase 1 consumes (a sniffed id simply misses in
    /// `SessionManager.get` — sniffed sessions have no inject transport);
    /// `reverse` is the lifecycle/heartbeat path keyed by what callers
    /// already hold.
    ///
    /// **Both maps are in-process only** — created empty in
    /// [`AiCoordRegistrar::new`], never rehydrated from the durable lifecycle
    /// store. A `reverse` hit therefore means "this runner process registered
    /// this session since boot", which is strictly narrower than "coord knows
    /// this session". Caller self-identification deliberately does NOT gate on
    /// it for that reason (it dropped 678 of 678 resolutions); see
    /// [`Self::session_id_for`].
    forward: Mutex<HashMap<Uuid, String>>,
    reverse: Mutex<HashMap<String, Uuid>>,
    /// Session-identity fabric Phase 1 — lifecycle store the registrar
    /// persists the coord-minted `fsh_` session handle into (next to the
    /// record's `claude_session_id`). Attached once at startup
    /// ([`AiCoordRegistrar::attach_lifecycle_store`]); unattached (tests,
    /// ephemeral registrars) → the handle mint/rebind is skipped entirely,
    /// which also keeps unit tests network-silent.
    lifecycle_store: OnceLock<Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>>,
    /// Test-only observability for the Phase-1 handle hook: counts every
    /// DECISION to fire it ([`AiCoordRegistrar::spawn_handle_register`]),
    /// incremented BEFORE the attached-store gate — so unit tests (which
    /// never attach a store and therefore stay network-silent) can still
    /// assert the fire/no-fire decision (review W3).
    #[cfg(test)]
    handle_hook_fires: std::sync::atomic::AtomicU64,
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
                lifecycle_store: OnceLock::new(),
                #[cfg(test)]
                handle_hook_fires: std::sync::atomic::AtomicU64::new(0),
            }),
        }
    }

    /// Attach the durable lifecycle store (once, at startup) so
    /// [`Self::register_session`] can acquire the coord-minted `fsh_` session
    /// handle and persist it next to the record's `claude_session_id`
    /// (session-identity fabric Phase 1). Without it the handle mint/rebind
    /// is skipped entirely (tests, ephemeral registrars).
    pub fn attach_lifecycle_store(
        &self,
        store: Arc<crate::session::session_lifecycle_store::SessionLifecycleStore>,
    ) {
        if self.inner.lifecycle_store.set(store).is_err() {
            warn!("ai_coord_register: lifecycle store already attached — ignoring");
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

    /// R4 — resolve a session key to its coord `session_id` (the durable
    /// handle). Consumed by the inject-audit / observability path.
    ///
    /// **The parameter is the R4/R6 index key — `claude_session_id` — for BOTH
    /// planes, and the two keyspaces are UNIFIED.** It reads like a bug at the
    /// pinned-plane call sites, which pass a `task_run_id`; it is not. The
    /// reverse map is keyed by `claude_session_id` (`register_inner` inserts
    /// `rev.insert(claude_session_id, …)`), and the runner pins every
    /// registrar-managed session's CLI session id to its `task_run_id`, so a
    /// pinned session's `task_run_id` *is* its `claude_session_id`. The sniffed
    /// plane passes the typed `--resume` id directly. Hence `session_key`, not
    /// `task_run_id` — the old name described one caller, not the key.
    ///
    /// **In-process and non-durable.** `reverse` is constructed empty in
    /// [`AiCoordRegistrar::new`] and is never rehydrated from the lifecycle
    /// store, so a hit means "**this runner process** registered this session
    /// **since boot**", NOT "this session is known to coord". Anything that
    /// needs the latter must consult a durable source — which is why the
    /// caller-self-identification path no longer uses this as a
    /// registered-ness filter (see `mcp_api::select_lifecycle_caller`).
    pub fn session_id_for(&self, session_key: &str) -> Option<Uuid> {
        self.inner
            .reverse
            .lock()
            .ok()
            .and_then(|g| g.get(session_key).copied())
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
    ///
    /// The runner pins every registrar-managed session's CLI session id to
    /// its `task_run_id`, so the index key here IS the session's
    /// `claude_session_id` — one keyspace shared with
    /// [`Self::register_sniffed_session`] (see [`Self::register_inner`]).
    pub fn register_session(
        &self,
        task_run_id: &str,
        purpose: &str,
        repo: Option<String>,
    ) -> Option<Uuid> {
        self.register_inner(task_run_id, Some(task_run_id), purpose, repo)
    }

    /// Session-identity fabric Phase 3 — register a SNIFFED interactive
    /// session (a `claude --resume <id>` / `--session-id <id>` line the
    /// operator typed into a plain terminal, lifted by
    /// [`crate::terminal::claude_resume_sniff`]) with coord, through the SAME
    /// machinery as [`Self::register_session`].
    ///
    /// Differences from the pinned plane, both derived from `task_run_id`
    /// being absent (there is no runner `SessionManager` entry for a typed
    /// session):
    ///
    /// - The `Started` payload carries NO `task_run_id`; instead it forwards
    ///   the durable anchor as `claude_code_session_id` so coord can join the
    ///   session row to commit history / transcripts.
    /// - `kind` is `terminal_claude`, NOT `agentic`. **Load-bearing:** coord's
    ///   `next_step.rs` auto-continuation targets only `agentic`/`workflow`
    ///   stale sessions — registering an OPERATOR'S interactive pane as
    ///   `agentic` would make coord eligible to autonomously "continue" it
    ///   when idle. `terminal_claude` is the existing coord-side kind for
    ///   exactly this plane and is never auto-continued.
    ///
    /// Dedupe is the same R6 index, keyed on `claude_session_id` — the
    /// pinned plane's `task_run_id` IS its `claude_session_id`, so the two
    /// planes share one keyspace and a session registered by either path
    /// no-ops in the other. The check-and-reserve is ATOMIC (one reverse-map
    /// lock acquisition, review W1), so even a restore storm racing N
    /// concurrent registrations of one session serializes to exactly one
    /// `Started` outbox row per distinct session per process lifetime
    /// (barring an outbox write failure, which rolls the reservation back
    /// for retry). Rows drain through the existing serialized `CoordSync`
    /// drain loop — zero direct HTTP from the sniff path.
    ///
    /// The Phase-1 handle hook at the end of registration fires as usual,
    /// minting/rebinding the `fsh_` handle with `task_run_id: None`. On an
    /// R6 hit it STILL fires (rebind is idempotent and re-points the
    /// handle's terminal_id at the new pane — review W3); the coord row's
    /// KIND, however, stays whatever the first registration wrote — a
    /// hand-resumed pinned session keeps `agentic` (kind transition needs
    /// coord-side support that doesn't exist; documented residual).
    pub fn register_sniffed_session(
        &self,
        claude_session_id: &str,
        purpose: &str,
        repo: Option<String>,
    ) -> Option<Uuid> {
        self.register_inner(claude_session_id, None, purpose, repo)
    }

    /// Shared register core for the pinned (`task_run_id = Some`) and sniffed
    /// (`task_run_id = None`) planes. `claude_session_id` is the R6 dedupe /
    /// R4 index key for BOTH — the durable anchor that always exists (plan
    /// `2026-07-05-session-identity-messaging-restore-fabric.md` §2.5).
    fn register_inner(
        &self,
        claude_session_id: &str,
        task_run_id: Option<&str>,
        purpose: &str,
        repo: Option<String>,
    ) -> Option<Uuid> {
        if !registration_enabled() {
            debug!(
                "ai_coord_register: disabled via QONTINUI_SESSION_AUTOMATION_REGISTER — skipping {}",
                claude_session_id
            );
            return None;
        }

        let session_id = crate::session::uuid_v7();

        // R6 — check-AND-reserve atomically under ONE reverse-map lock
        // acquisition (review W1). `spawn_register_typed_resume` dispatches a
        // detached task per submitted PTY line, so a restore storm can race N
        // registrations of the SAME claude_session_id; a check-then-act gap
        // here would let several racers each write a Started row and orphan
        // the losers' coord rows (their reverse-map entries overwritten,
        // never closable). Reserving the freshly-minted id in the same
        // critical section guarantees exactly one winner proceeds to the
        // outbox write; losers observe the reservation and no-op with the
        // winner's id. The reservation is rolled back below if the outbox
        // write fails, so a later retry can re-register.
        let dedupe_hit = {
            let Ok(mut rev) = self.inner.reverse.lock() else {
                warn!(
                    "ai_coord_register: reverse-index lock poisoned — skipping {}",
                    claude_session_id
                );
                return None;
            };
            match rev.get(claude_session_id) {
                Some(existing) => Some(*existing),
                None => {
                    rev.insert(claude_session_id.to_string(), session_id);
                    None
                }
            }
        };
        if let Some(existing) = dedupe_hit {
            if task_run_id.is_none() {
                // Sniffed-plane R6 hit (review W3): the operator re-typed a
                // resume line for an already-registered session — most often
                // a hand-resume of a PINNED session (`claude --resume
                // <task_run_id>`), which keeps the coord row's original kind
                // (`agentic`; a coord-side kind transition needs server
                // support that doesn't exist — documented residual). Fire
                // the handle hook anyway: the server-side rebind is
                // idempotent and refreshes the handle's terminal_id to the
                // NEW pane (the sniff's record_open ran before this call).
                // Info (not debug): this is a plane-crossing event worth
                // seeing in the log.
                info!(
                    "ai_coord_register: sniffed re-register of {} (already coord session {}) — \
                     no new row; refiring handle rebind for the new terminal",
                    claude_session_id, existing
                );
                self.spawn_handle_register(
                    claude_session_id,
                    existing,
                    None,
                    super::session_handle::name_alias(purpose),
                );
            } else {
                debug!(
                    "ai_coord_register: {} already registered as coord session {} — no-op",
                    claude_session_id, existing
                );
            }
            return Some(existing);
        }

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
        // Captured before `purpose` moves into the intent JSON — forwarded as
        // the fabric handle's human `name` alias below.
        let name_alias = super::session_handle::name_alias(&purpose);
        let mut intent = json!({ "purpose": purpose });
        if let Some(r) = repo.as_ref() {
            intent["repo"] = json!(r);
        }

        // The `Started` payload is the create-body shape `rebuild_create_body`
        // relabels for `POST /sessions`. `tenant_id` is omitted (nil) so coord
        // resolves the real tenant from the device registration — the same path
        // every runner-originated session uses. Kind: `agentic` for the pinned
        // plane, `terminal_claude` for sniffed interactive panes (see
        // `register_sniffed_session` — coord must never auto-continue an
        // operator's own terminal).
        let kind = if task_run_id.is_some() {
            SessionKind::Agentic
        } else {
            SessionKind::TerminalClaude
        };
        let mut payload = json!({
            "id": session_id,
            "kind": kind.as_str(),
            "intent": intent,
            "state": "active",
            "started_at": now,
        });
        if let Some(trid) = task_run_id {
            // Pinned plane: forward the SessionManager key so coord persists
            // `coord.sessions.task_run_id` for inject-target resolution.
            payload["task_run_id"] = json!(trid);
        }
        // BOTH planes forward the durable Claude session id
        // (`rebuild_create_body` passes it through as a first-class field;
        // coord tolerates its absence elsewhere). This is what makes the
        // session ADDRESSABLE: coord's `create_session` upserts
        // `coord.agent_sessions(id = claude_code_session_id, device_id)`, and
        // that row is what the mailbox scopes on and what the fabric's
        // handle register must present as `agent_session_id`
        // (`session_on_device`, fail-closed).
        //
        // It used to be sent on the SNIFFED plane only, so pinned/agentic
        // sessions had a `coord.sessions` row and NO `coord.agent_sessions`
        // row — unaddressable, and their handle register refused 403.
        //
        // Guarded on the anchor parsing as a UUID: coord types the field
        // `Option<Uuid>`, so a non-uuid anchor would fail deserialization of
        // the WHOLE create body and break session registration itself. A
        // non-uuid anchor simply omits the field, exactly as before.
        if uuid::Uuid::parse_str(claude_session_id.trim()).is_ok() {
            payload["claude_code_session_id"] = json!(claude_session_id);
        }

        if let Err(e) = self.inner.outbox.record(
            self.inner.machine_id,
            session_id,
            SessionEventKind::Started,
            payload,
        ) {
            warn!(
                "ai_coord_register: outbox Started write failed for {} (best-effort): {}",
                claude_session_id, e
            );
            // Roll back the W1 reservation (only if it is still ours — a
            // concurrent close_session can't have replaced it, but guard
            // anyway) so a later retry can re-register.
            if let Ok(mut rev) = self.inner.reverse.lock() {
                if rev.get(claude_session_id) == Some(&session_id) {
                    rev.remove(claude_session_id);
                }
            }
            return None;
        }

        // Finalize the R4 index: the reverse entry was reserved atomically
        // above; add the forward entry now that the outbox write is durable.
        if let Ok(mut fwd) = self.inner.forward.lock() {
            fwd.insert(session_id, claude_session_id.to_string());
        }

        info!(
            "ai_coord_register: registered AI session {} as coord session {} (kind={})",
            claude_session_id,
            session_id,
            kind.as_str()
        );

        // Session-identity fabric Phase 1 — acquire/rebind the stable `fsh_`
        // session handle for this session. `claude_session_id` is the durable
        // anchor the registry mints/rebinds on — for the pinned plane it
        // equals `task_run_id` (each call site passes `CliSessionContext {
        // cli_session_id: task_run_id, .. }`); for the sniffed plane it is
        // the id lifted from the typed `--resume`/`--session-id` flag and
        // `task_run_id` rides as `None`. Running on every FRESH registration
        // also covers restore for free: a restarted runner re-registers each
        // resumed session (the R4 dedup index is in-memory, lost on restart),
        // and the server-side rebind keyed on `claude_session_id` refreshes
        // `current_agent_session_id` on the existing handle row.
        self.spawn_handle_register(claude_session_id, session_id, task_run_id, name_alias);

        Some(session_id)
    }

    /// Fire the Phase-1 `fsh_` handle mint/rebind for `claude_session_id`,
    /// best-effort on a detached thread (NEVER fails the caller; coord may not
    /// serve the route yet). Gated on an ATTACHED lifecycle store (main.rs
    /// attaches at boot) — which also keeps unit-test registrars (never
    /// attached) network-silent. Shared by the fresh-registration path and the
    /// sniffed-plane R6-hit rebind (review W3).
    ///
    /// `coord_session_id` is this boot's `coord.sessions.id` — carried for the
    /// correlation log ONLY. It is deliberately NOT the wire
    /// `agent_session_id`: coord gates that field fail-closed through
    /// `session_on_device` against `coord.agent_sessions`, where the
    /// per-boot `coord.sessions` uuid never appears, so presenting it earned a
    /// `403` on every call and left the registry empty. The addressable id is
    /// the anchor itself — see `session_handle`'s module docs for the
    /// both-sides-of-the-wire proof.
    fn spawn_handle_register(
        &self,
        claude_session_id: &str,
        coord_session_id: Uuid,
        task_run_id: Option<&str>,
        name: Option<String>,
    ) {
        // Counted BEFORE the store gate so network-silent unit tests can
        // observe the fire decision (see `Inner::handle_hook_fires`).
        #[cfg(test)]
        self.inner
            .handle_hook_fires
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let Some(agent_session_id) =
            super::session_handle::agent_session_id_for_anchor(claude_session_id)
        else {
            return; // non-uuid anchor — counted inside, never silent
        };
        if let Some(store) = self.inner.lifecycle_store.get().cloned() {
            let record = store.get(claude_session_id);
            let terminal_id = record
                .as_ref()
                .map(|r| r.terminal_id.clone())
                .filter(|t| !t.trim().is_empty());
            // §7's precondition: a live runner terminal backs this handle.
            // The store's view is the honest one available here — an OPEN
            // record with a terminal bound. A session with no record at all
            // (an AI subprocess with no terminal-grid row) is NOT promptable.
            let promptable = record
                .as_ref()
                .is_some_and(|r| r.state == "open" && !r.terminal_id.trim().is_empty());
            debug!(
                claude_session_id,
                coord_session_id = %coord_session_id,
                agent_session_id = %agent_session_id,
                promptable,
                "ai_coord_register: firing fabric handle register"
            );
            super::session_handle::spawn_register(
                store,
                super::session_handle::HandleRegisterRequest {
                    claude_session_id: claude_session_id.to_string(),
                    agent_session_id,
                    task_run_id: task_run_id.map(str::to_string),
                    terminal_id,
                    name,
                    machine_id: Some(self.inner.machine_id),
                    promptable,
                },
            );
        }
    }

    /// Test-only: how many times the Phase-1 handle hook fired (decision
    /// count, pre-store-gate — see `Inner::handle_hook_fires`).
    #[cfg(test)]
    fn handle_hook_fire_count(&self) -> u64 {
        self.inner
            .handle_hook_fires
            .load(std::sync::atomic::Ordering::Relaxed)
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
    ///
    /// `session_key` is whatever key the matching registration wrote into the
    /// reverse index — a `task_run_id` OR a `claude_session_id`, and BOTH are
    /// correct. The registrar deliberately unions the two keyspaces:
    /// `register_session` passes a `task_run_id` into `register_inner`'s
    /// `claude_session_id` slot while `register_sniffed_session` passes a real
    /// `claude_session_id` (rationale at the `register_inner` doc, "Shared
    /// register core … `claude_session_id` is the R6 dedupe / R4 index key for
    /// BOTH"). So this parameter is NOT named for either plane — the two live
    /// caller classes prove it: `commands::ai_session` / `loop_controller` pass
    /// a `task_run_id`, and `main.rs`'s lifecycle-store close observer passes a
    /// `claude_session_id`. A key from neither plane is an index miss, which is
    /// the documented no-op.
    pub fn close_session(&self, session_key: &str) {
        let session_id = {
            // Evict reverse first, capturing the coord id.
            let Some(id) = self
                .inner
                .reverse
                .lock()
                .ok()
                .and_then(|mut g| g.remove(session_key))
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
                session_key, e
            );
        } else {
            info!(
                "ai_coord_register: closed AI session {} (coord {})",
                session_key, session_id
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
// ## One service for the whole process, not one thread per session
//
// Every [`AgentLogEmitter`] handle is a clone of ONE `std::sync::mpsc` sender
// feeding ONE drain thread (`agent-log-emitter`), started lazily on the first
// `start()` and kept in a `OnceLock` for the life of the process. That thread
// owns the single `reqwest::blocking::Client` (built on the thread itself —
// a blocking client is a whole tokio runtime, and callers can be inside one)
// and a `HashMap<agent_id, VecDeque<LogEntry>>` of per-agent FIFO queues.
// Batching is PER AGENT with the caps below: a queue that reaches
// `MAX_AGENT_LOG_BATCH` flushes at once, every non-empty queue flushes on the
// `AGENT_LOG_FLUSH_INTERVAL` tick, a failed POST requeues its batch to the
// front, and a queue that drains empty is REMOVED from the map — so the map
// is bounded by the agents that produced a line in the last tick, not by the
// number of sessions the process has ever hosted. `close()` enqueues the
// `session_closed` line and a typed `Close(agent_id)`, which the service
// honours by flushing that agent once more and dropping its queue whatever
// the flush's outcome.
//
// Why this shape: the design it replaced spawned an OS thread AND a private
// blocking client (its own tokio runtime, epoll fd and eventfds) per session,
// and that thread could only exit on `Close` or channel disconnect — a
// `Close` its coalescing loop pulled off the channel and discarded, and a
// disconnect the transcript watcher never produced because it holds its
// handle for the tail's whole lifetime. Measured on a runner with 0 live
// sessions: 154 `agent-log-emit-*` threads paired 1:1 with 154
// `reqwest-internal-sync-runtime` threads (plan
// `2026-08-28-runner-thread-and-socket-leak-wedges-the-accept-path`). The gauge published on
// `/health` as `agent_log_emitter_agents` is the live count of per-agent
// queues, refreshed by the service after every pass.
//
// Everything here is gated on [`agent_logs_from_sessions_enabled`] (default
// ON) at the construction site, so an OFF gate means no handle is ever built,
// the service thread never starts, and zero coord traffic is added.

/// Max entries per `POST /agents/:id/log` batch. Mirrors coord's
/// `agent_logs::MAX_BATCH_SIZE`; coord 400s a larger batch.
const MAX_AGENT_LOG_BATCH: usize = 500;

/// Periodic flush cadence of the emitter service: every agent with a
/// non-empty queue is flushed once per tick.
const AGENT_LOG_FLUSH_INTERVAL: Duration = Duration::from_secs(5);

/// Upper bound on buffered entries PER AGENT when coord is unreachable. Older
/// entries drop FIFO so an offline coord can't grow a queue without bound.
/// Generous (10×batch) since each entry is small.
const AGENT_LOG_QUEUE_CAP: usize = MAX_AGENT_LOG_BATCH * 10;

/// Upper bound on buffered entries across ALL agents. The per-agent cap bounds
/// one chatty session; this bounds the process, so a coord outage while the
/// transcript watcher holds a hundred tails cannot retain a hundred full
/// queues. Over budget, the oldest entry of the largest queue is dropped.
const AGENT_LOG_TOTAL_CAP: usize = MAX_AGENT_LOG_BATCH * 40;

/// How long the service stops attempting POSTs after a transport failure
/// (timeout, refused, DNS). One flush interval: the next tick re-probes with a
/// single request instead of paying the client timeout once per agent.
const AGENT_LOG_BACKOFF: Duration = AGENT_LOG_FLUSH_INTERVAL;

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

/// Messages every handle pushes to the one emitter service.
enum EmitMsg {
    /// A fully-formed log entry to enqueue under its own `agent_session_id`.
    Entry(LogEntry),
    /// This agent is done: flush its queue one final time, then drop the
    /// queue — whatever the flush's outcome.
    Close(Uuid),
}

/// How the service learns where coord is. A function pointer rather than a
/// direct call so a test can build a service that resolves NO coord base and
/// therefore never opens a socket, while production passes
/// [`qontinui_runner_lib::profiles::connected_coord_base`].
type CoordBaseResolver = fn() -> Option<String>;

/// Live per-agent queues held by the PROCESS-WIDE emitter service — the value
/// [`agent_log_emitter_agents`] reads and `/health` publishes.
static AGENT_LOG_EMITTER_AGENTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Number of agents whose log queue the process-wide emitter service currently
/// holds — bounded by the agents that produced a line since the last flush
/// that drained them, plus any whose coord POSTs are failing. Published on
/// `/health` as `agent_log_emitter_agents`; `0` before the service has ever
/// started as well as when it is idle.
pub(crate) fn agent_log_emitter_agents() -> usize {
    AGENT_LOG_EMITTER_AGENTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The one drain thread plus the sender every handle clones. Constructed once
/// per process by [`global_emitter_service`]; constructed per test by the
/// tests, with a resolver that answers `None`.
struct EmitterService {
    tx: std::sync::mpsc::Sender<EmitMsg>,
}

impl EmitterService {
    /// Spawn the drain thread, which refreshes `live_agents` after every pass
    /// (the process-wide instance passes [`AGENT_LOG_EMITTER_AGENTS`]; a test
    /// passes its own so tests never read another test's queues). `None` only
    /// when the OS refuses the thread, in which case nothing is cached and the
    /// next `start()` tries again — pinning a spawn failure for the process
    /// lifetime would silently switch emission off forever after one moment of
    /// thread exhaustion.
    fn spawn(
        coord_base: CoordBaseResolver,
        live_agents: &'static std::sync::atomic::AtomicUsize,
    ) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<EmitMsg>();
        std::thread::Builder::new()
            .name("agent-log-emitter".to_string())
            .spawn(move || run_emitter_service(rx, coord_base, live_agents))
            .map_err(|e| warn!("agent_log_emitter: failed to spawn the emitter service: {e}"))
            .ok()?;
        Some(Self { tx })
    }
}

/// The process-wide service, started on first use. Guarded by a mutex around
/// the spawn so two sessions starting at once cannot each spawn a thread and
/// discard one — a `OnceLock::get_or_init` cannot express "spawning may fail",
/// and a lost race would leak exactly the thread this design exists to stop.
fn global_emitter_service() -> Option<&'static EmitterService> {
    static SERVICE: OnceLock<EmitterService> = OnceLock::new();
    static SPAWN: Mutex<()> = Mutex::new(());
    if let Some(s) = SERVICE.get() {
        return Some(s);
    }
    let _spawning = SPAWN
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(s) = SERVICE.get() {
        return Some(s);
    }
    let service = EmitterService::spawn(
        qontinui_runner_lib::profiles::connected_coord_base,
        &AGENT_LOG_EMITTER_AGENTS,
    )?;
    Some(SERVICE.get_or_init(|| service))
}

/// Cheap-to-clone handle a session pushes its log entries through. Owns no
/// thread and no client: `tx` is a clone of the process-wide service's single
/// sender, so building, cloning, and dropping handles costs nothing beyond the
/// channel refcount, and a handle that is never `close()`d leaves nothing
/// behind once its last queued line has flushed.
#[derive(Clone)]
pub struct AgentLogEmitter {
    tx: std::sync::mpsc::Sender<EmitMsg>,
    /// The coord session id — both the URL `agent_id` and the body
    /// `agent_session_id`, and the key the service queues this handle's
    /// entries under. Carried so emit helpers can stamp it without the caller
    /// re-threading it.
    agent_id: Uuid,
    device_id: Option<Uuid>,
    /// Phase 8b item 7 — the owning session's tenant binding, resolved once
    /// at handle start (device DEFAULT for these runner-managed sessions)
    /// and stamped on every entry.
    tenant_id: Option<Uuid>,
}

impl AgentLogEmitter {
    /// Build a handle for the coord session `agent_id`, reading `device_id`
    /// once from `~/.qontinui/machine.json`. Returns `None` (no handle, no
    /// traffic) when the gate is OFF or `device_id` is unreadable — both are
    /// fail-safe no-ops: coord's Phase-1a tenant fallback needs the
    /// `device_id`, so emitting without it is pointless. The first successful
    /// call in the process starts the emitter service; later calls only clone
    /// its sender.
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
        let service = global_emitter_service()?;

        info!(
            "agent_log_emitter: streaming session {} to coord agent_logs",
            agent_id
        );
        Some(Self::with_service(
            service,
            agent_id,
            Some(device_id),
            // Phase 8b item 7 — resolve the owning session's binding once
            // (device DEFAULT: these are runner-managed operator sessions;
            // coord-spawned agent sessions don't stream through this
            // emitter). Best-effort: `None` omits the field on the wire.
            crate::fleet::resolve_tenant_id(),
        ))
    }

    /// Build a handle on an explicit service with an explicit identity — no
    /// gate, no `machine.json`, no tenant resolution. `start()` funnels through
    /// this; tests call it directly against a service whose coord base
    /// resolver answers `None`.
    fn with_service(
        service: &EmitterService,
        agent_id: Uuid,
        device_id: Option<Uuid>,
        tenant_id: Option<Uuid>,
    ) -> Self {
        Self {
            tx: service.tx.clone(),
            agent_id,
            device_id,
            tenant_id,
        }
    }

    /// Build a `LogEntry` stamped with this handle's identity (agent session
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
    /// `payload`, stamped with this handle's identity. Used by callers that
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

    /// Terminal `session_closed` milestone, then ask the service to flush this
    /// agent one final time and drop its queue. The queue is keyed by
    /// `agent_id` and shared by every clone of this handle, so one clone's
    /// close detaches it for all of them. Idempotent-safe: a second close
    /// enqueues another (harmless) terminal line and a second `Close` that
    /// finds no queue. An entry arriving after the close simply opens a fresh
    /// queue.
    pub fn close(&self) {
        let _ = self
            .tx
            .send(EmitMsg::Entry(self.entry("info", "session_closed", None)));
        let _ = self.tx.send(EmitMsg::Close(self.agent_id));
    }
}

/// Read `device_id` from `~/.qontinui/machine.json` and parse it as a `Uuid`.
/// Reuses the canonical disk reader in [`crate::pair`]. Any failure (missing
/// file, unparseable, non-UUID) yields `None` — the handle then skips.
fn read_device_id_uuid() -> Option<Uuid> {
    // `pair` lives in the `qontinui_runner_lib` crate (this module compiles into
    // both the lib and the bin target, so the bin can't reach it via `crate::`).
    let raw = qontinui_runner_lib::pair::read_device_id_from_disk().ok()?;
    Uuid::parse_str(raw.trim()).ok()
}

/// Per-agent FIFO queues keyed by coord session id.
type AgentQueues = HashMap<Uuid, std::collections::VecDeque<LogEntry>>;

/// The service's one thread. Blocks on the channel until a message arrives or
/// the flush tick is due, absorbs the whole ready burst into the per-agent
/// queues, then flushes: agents whose queue reached a batch flush immediately;
/// closed agents flush once and are dropped; on a tick every remaining
/// non-empty queue flushes and the ones that drained empty are removed. The
/// gauge is refreshed last so `/health` reads the post-pass map.
///
/// ## Transport circuit breaker
///
/// Every POST runs on this one thread, so with coord unreachable by TIMEOUT
/// (blackholed, DNS stall) a naive pass would pay the client timeout once per
/// agent — a hundred watcher tails is minutes during which the thread is not
/// on `recv` and the unbounded channel absorbs every session's lines. So the
/// first transport failure in a pass ends that pass's flushing and opens the
/// breaker for [`AGENT_LOG_BACKOFF`]; queues are retained (capped) and the
/// next tick re-probes with a single request. A rejection coord ANSWERED
/// (non-2xx) is not a transport failure and does not trip it.
fn run_emitter_service(
    rx: std::sync::mpsc::Receiver<EmitMsg>,
    coord_base: CoordBaseResolver,
    live_agents: &'static std::sync::atomic::AtomicUsize,
) {
    let mut queues: AgentQueues = HashMap::new();
    // Built on THIS thread, lazily, on the first flush that has somewhere to
    // send: a `reqwest::blocking::Client` is a tokio runtime with its own
    // thread, and a runner whose coord base never resolves should not own one.
    let mut client: Option<reqwest::blocking::Client> = None;
    let mut next_tick = std::time::Instant::now() + AGENT_LOG_FLUSH_INTERVAL;
    let mut breaker_open_until: Option<std::time::Instant> = None;

    loop {
        let wait = next_tick.saturating_duration_since(std::time::Instant::now());
        let first = match rx.recv_timeout(wait) {
            Ok(msg) => Some(msg),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let mut full: Vec<Uuid> = Vec::new();
        let mut closed: Vec<(Uuid, std::collections::VecDeque<LogEntry>)> = Vec::new();
        if let Some(msg) = first {
            absorb(&mut queues, msg, &mut full, &mut closed);
            while let Ok(msg) = rx.try_recv() {
                absorb(&mut queues, msg, &mut full, &mut closed);
            }
        }

        let now = std::time::Instant::now();
        let tick_due = now >= next_tick;
        let breaker_open = breaker_open_until.is_some_and(|until| now < until);
        // Resolve the coord base once per pass, and only on a pass that will
        // flush something — resolution reads the profile store. A closed
        // breaker skips every flush this pass; closed queues are still dropped.
        let base = if breaker_open || (full.is_empty() && closed.is_empty() && !tick_due) {
            None
        } else {
            coord_base()
        };

        // `true` while this pass may keep sending; flips on the first
        // transport failure and opens the breaker. `send` returns whether it
        // attempted a flush at all, so a caller can tell "sent or answered"
        // from "the breaker was already open".
        let mut sending = base.is_some();
        let mut send = |queue: &mut std::collections::VecDeque<LogEntry>, agent_id: Uuid| -> bool {
            if !sending {
                return false;
            }
            if flush_batch(&mut client, base.as_deref(), agent_id, queue) == Flush::TransportFailed
            {
                sending = false;
                breaker_open_until = Some(std::time::Instant::now() + AGENT_LOG_BACKOFF);
            }
            true
        };

        for agent_id in full {
            if let Some(queue) = queues.get_mut(&agent_id) {
                send(queue, agent_id);
            }
        }
        for (agent_id, mut queue) in closed {
            let attempted = send(&mut queue, agent_id);
            if !queue.is_empty() {
                if attempted {
                    // A flush ran: one batch was sent, or coord answered and it
                    // was requeued, or this very call was the transport failure
                    // that opened the breaker (requeued intact). Whatever is
                    // still queued goes with the agent.
                    debug!(
                        "agent_log_emitter: dropped {} entries still queued for closed agent {} \
                         after its final flush",
                        queue.len(),
                        agent_id
                    );
                } else if base.is_none() && !breaker_open {
                    // No coord base resolves — a standalone or unpaired runner.
                    // Ordinary, not a fault: nothing was ever going to be sent.
                    debug!(
                        "agent_log_emitter: dropped {} entries for closed agent {} — no coord \
                         base resolved",
                        queue.len(),
                        agent_id
                    );
                } else {
                    // The breaker withheld the final flush — opened earlier this
                    // pass or by a previous one. Visible at default level so a
                    // run of these reads as the outage it is.
                    warn!(
                        "agent_log_emitter: dropped {} unsent entries for closed agent {} \
                         without a final flush — the transport breaker is open",
                        queue.len(),
                        agent_id
                    );
                }
            }
        }
        if tick_due {
            for (agent_id, queue) in queues.iter_mut() {
                send(queue, *agent_id);
            }
            next_tick = std::time::Instant::now() + AGENT_LOG_FLUSH_INTERVAL;
        }
        queues.retain(|_, queue| !queue.is_empty());

        live_agents.store(queues.len(), std::sync::atomic::Ordering::Relaxed);
    }

    // Every sender is gone: nothing can arrive, so flush what is left once.
    let base = coord_base();
    for (agent_id, queue) in queues.iter_mut() {
        if flush_batch(&mut client, base.as_deref(), *agent_id, queue) == Flush::TransportFailed {
            break;
        }
    }
    live_agents.store(0, std::sync::atomic::Ordering::Relaxed);
    debug!("agent_log_emitter: emitter service stopped");
}

/// Apply one message to the queues. An entry lands at the back of its agent's
/// queue (opening one if needed, dropping the oldest at the cap) and marks the
/// agent `full` once the queue reaches a batch. A `Close` DETACHES the agent's
/// queue from the map into `closed` right here, at absorb time, so message
/// order inside one coalesced burst is honoured: an entry that follows the
/// close opens a fresh queue instead of being dropped with the closed one.
/// Both lists are acted on by the caller after coalescing, so a burst of
/// closes costs one pass. A `Close` for an agent with no queue is a no-op.
fn absorb(
    queues: &mut AgentQueues,
    msg: EmitMsg,
    full: &mut Vec<Uuid>,
    closed: &mut Vec<(Uuid, std::collections::VecDeque<LogEntry>)>,
) {
    match msg {
        EmitMsg::Entry(entry) => {
            // Every handle stamps its own id via `AgentLogEmitter::entry`; an
            // unstamped entry has no queue to live in and no URL to go to.
            let Some(agent_id) = entry.agent_session_id else {
                warn!(
                    "agent_log_emitter: dropping entry {:?} with no agent_session_id",
                    entry.event
                );
                return;
            };
            let queue = queues.entry(agent_id).or_default();
            if queue.len() >= AGENT_LOG_QUEUE_CAP {
                queue.pop_front();
            }
            queue.push_back(entry);
            if queue.len() >= MAX_AGENT_LOG_BATCH && !full.contains(&agent_id) {
                full.push(agent_id);
            }
            enforce_total_cap(queues);
        }
        EmitMsg::Close(agent_id) => {
            if let Some(queue) = queues.remove(&agent_id) {
                closed.push((agent_id, queue));
            }
        }
    }
}

/// Keep the sum of all queues at or under [`AGENT_LOG_TOTAL_CAP`] by dropping
/// the oldest entry of the largest queue — the session that is producing the
/// most is the one that loses one line, and a per-agent cap alone would let
/// many moderately busy agents hold an unbounded total during an outage.
fn enforce_total_cap(queues: &mut AgentQueues) {
    let mut total: usize = queues.values().map(std::collections::VecDeque::len).sum();
    while total > AGENT_LOG_TOTAL_CAP {
        let Some(largest) = queues.values_mut().max_by_key(|queue| queue.len()) else {
            return;
        };
        largest.pop_front();
        total -= 1;
    }
}

/// What one [`flush_batch`] did, so the service can tell "coord answered"
/// (keep going) from "coord did not answer" (stop for this pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// Nothing to send, no coord base, or no client — no request was made.
    Skipped,
    /// coord answered: the batch was accepted, or rejected and requeued.
    Answered,
    /// The request itself failed (timeout, refused, DNS): requeued, and the
    /// caller should not try another agent this pass.
    TransportFailed,
}

/// Drain up to `MAX_AGENT_LOG_BATCH` entries from the front of `queue` and POST
/// them as one array to `{base}/agents/{agent_id}/log`. On failure (no coord
/// base, client build failed, HTTP error) the drained slice is pushed back to
/// the FRONT in order so nothing is lost and the next tick retries — bounded
/// by the queue cap. Best-effort: never panics, never blocks a session. The
/// returned [`Flush`] tells the service whether coord answered at all.
fn flush_batch(
    client: &mut Option<reqwest::blocking::Client>,
    base: Option<&str>,
    agent_id: Uuid,
    queue: &mut std::collections::VecDeque<LogEntry>,
) -> Flush {
    if queue.is_empty() {
        return Flush::Skipped;
    }
    let Some(base) = base else {
        return Flush::Skipped; // coord not configured — keep buffering (capped).
    };
    if client.is_none() {
        *client = reqwest::blocking::Client::builder()
            // Connect separately from total: a blackholed coord should cost
            // this thread 2 s per probe, not the full request budget.
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| warn!("agent_log_emitter: failed to build the HTTP client: {e}"))
            .ok();
    }
    let Some(client) = client.as_ref() else {
        return Flush::Skipped; // client build failed — keep buffering (capped), retry next flush.
    };

    let take = queue.len().min(MAX_AGENT_LOG_BATCH);
    let batch: Vec<LogEntry> = queue.drain(..take).collect();

    let url = format!("{base}/agents/{agent_id}/log");
    // Blocking sibling of the async helper — same token source (the default
    // device-JWT slot this function was already reading by hand), same
    // never-fatal posture, and now the same `DATA_PLANE_TOTAL`/`AUTHED`
    // counters. This is a per-tick agent-log batcher, i.e. one of the highest-
    // rate coord writers in the process; hand-rolling the header kept it out of
    // the coverage readout entirely, which is the exact defect that readout
    // exists to expose.
    //
    // `Device`: the route resolves the row's tenant from the path `agent_id`
    // (coord's `resolve_tenant_from_agent_id`) and never reads the bearer, so
    // no slot choice can move the row. Not `Unresolved` — nothing failed here.
    let req = crate::auth::attach_device_auth_blocking(
        client.post(&url).json(&batch),
        crate::auth::TenantScope::Device,
    );

    match req.send() {
        Ok(resp) if resp.status().is_success() => {
            debug!(
                "agent_log_emitter: flushed {} entries for {}",
                batch.len(),
                agent_id
            );
            Flush::Answered
        }
        Ok(resp) => {
            warn!(
                "agent_log_emitter: POST /agents/{}/log returned {} — requeueing {} entries",
                agent_id,
                resp.status(),
                batch.len()
            );
            requeue_front(queue, batch);
            Flush::Answered
        }
        Err(e) => {
            debug!(
                "agent_log_emitter: POST /agents/{}/log failed ({e}) — requeueing {} entries",
                agent_id,
                batch.len()
            );
            requeue_front(queue, batch);
            Flush::TransportFailed
        }
    }
}

/// Push a failed batch back to the FRONT of the queue in original order,
/// trimming the head (oldest) to honor `AGENT_LOG_QUEUE_CAP` — the same policy
/// `absorb` applies.
fn requeue_front(queue: &mut std::collections::VecDeque<LogEntry>, batch: Vec<LogEntry>) {
    for e in batch.into_iter().rev() {
        queue.push_front(e);
    }
    while queue.len() > AGENT_LOG_QUEUE_CAP {
        queue.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::env_lock;
    use tempfile::tempdir;

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
    fn pinned_register_also_forwards_claude_code_session_id() {
        // The addressability fix: coord's `create_session` upserts
        // `coord.agent_sessions(id = claude_code_session_id, device_id)`, and
        // that row is what the mailbox scopes on AND what the fabric handle
        // register must present as `agent_session_id`. Sending it only on the
        // sniffed plane left pinned/agentic sessions unaddressable and their
        // handle register refused 403.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let trid = Uuid::new_v4().to_string();

        reg.register_session(&trid, "purpose", None).unwrap();

        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        // Pinned plane keeps its task_run_id AND now carries the anchor.
        assert_eq!(pending[0].payload["task_run_id"], json!(trid));
        assert_eq!(pending[0].payload["claude_code_session_id"], json!(trid));
    }

    #[test]
    fn non_uuid_anchor_omits_claude_code_session_id() {
        // coord types the field `Option<Uuid>`, so forwarding a non-uuid
        // anchor would fail deserialization of the WHOLE create body and
        // break session registration itself. Omit instead.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();

        reg.register_sniffed_session("not-a-uuid", "Terminal 9", None)
            .unwrap();

        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0].payload.get("claude_code_session_id").is_none(),
            "a non-uuid anchor must be omitted, not sent and rejected"
        );
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
    fn sniffed_register_writes_terminal_claude_row_without_task_run_id() {
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let csid = Uuid::new_v4().to_string();

        let coord_id = reg
            .register_sniffed_session(&csid, "Terminal 3", None)
            .unwrap();

        // Same R4 index, keyed on claude_session_id.
        assert_eq!(
            reg.task_run_id_for(&coord_id).as_deref(),
            Some(csid.as_str())
        );
        assert_eq!(reg.session_id_for(&csid), Some(coord_id));

        // Exactly one Started row: kind terminal_claude (NEVER agentic — coord
        // auto-continues stale agentic sessions, and this is an operator
        // pane), NO task_run_id, and the durable anchor forwarded as
        // claude_code_session_id.
        let pending = reg.inner.outbox.pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_kind, SessionEventKind::Started.as_str());
        assert_eq!(pending[0].payload["kind"], json!("terminal_claude"));
        assert!(pending[0].payload.get("task_run_id").is_none());
        assert_eq!(pending[0].payload["claude_code_session_id"], json!(csid));
        assert_eq!(pending[0].payload["id"], json!(coord_id));
    }

    #[test]
    fn sniffed_reregister_is_idempotent_no_duplicate_started_row() {
        // R6 for the sniffed plane: a re-sniff of the same live session (the
        // operator re-types the resume line, or a restore storm re-observes
        // it) must not re-register — one Started row per session per process.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let csid = Uuid::new_v4().to_string();

        let first = reg
            .register_sniffed_session(&csid, "Terminal 1", None)
            .unwrap();
        let second = reg
            .register_sniffed_session(&csid, "Terminal 1", None)
            .unwrap();
        assert_eq!(first, second, "re-sniff returns the same coord id");

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
    fn sniffed_and_pinned_planes_share_one_dedupe_keyspace() {
        // The pinned plane's task_run_id IS its claude_session_id, so a
        // session that registered as pinned must no-op when the sniff later
        // observes the same id (and vice versa) — no duplicate coord row.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let id = Uuid::new_v4().to_string();

        let pinned = reg.register_session(&id, "purpose", None).unwrap();
        let sniffed = reg
            .register_sniffed_session(&id, "Terminal 2", None)
            .unwrap();
        assert_eq!(pinned, sniffed, "one coord identity across both planes");
        assert_eq!(reg.inner.outbox.pending().unwrap().len(), 1);
    }

    #[test]
    fn sniffed_disabled_gate_registers_nothing() {
        let _env = env_lock();
        std::env::set_var("QONTINUI_SESSION_AUTOMATION_REGISTER", "0");
        let (reg, _dir) = registrar();
        let csid = Uuid::new_v4().to_string();
        assert!(reg
            .register_sniffed_session(&csid, "Terminal 1", None)
            .is_none());
        assert!(reg.inner.outbox.pending().unwrap().is_empty());
        assert!(reg.session_id_for(&csid).is_none());
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
    }

    #[test]
    fn concurrent_sniffed_registers_serialize_to_one_started_row() {
        // Review W1: `spawn_register_typed_resume` dispatches a detached task
        // per typed line, so a restore storm can race N registrations of ONE
        // claude_session_id. The atomic check-and-reserve must serialize
        // them: whatever the interleaving, exactly one Started row is
        // written and every racer returns the same coord id.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let csid = Uuid::new_v4().to_string();

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let ids: Vec<Option<Uuid>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let reg = reg.clone();
                    let csid = csid.clone();
                    let barrier = barrier.clone();
                    s.spawn(move || {
                        barrier.wait();
                        reg.register_sniffed_session(&csid, "Terminal 1", None)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        let first = ids[0].expect("racer 0 must resolve a coord id");
        assert!(
            ids.iter().all(|i| *i == Some(first)),
            "every racer must observe the same coord id: {ids:?}"
        );
        let started = reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Started.as_str())
            .count();
        assert_eq!(started, 1, "exactly one Started row despite the race");
        assert_eq!(reg.session_id_for(&csid), Some(first));
        assert_eq!(reg.task_run_id_for(&first).as_deref(), Some(csid.as_str()));
    }

    #[test]
    fn handle_hook_fires_on_fresh_register_and_sniffed_r6_hit_only() {
        // Review W3: a sniffed R6 hit (hand-resume of an already-registered
        // session) must REFIRE the handle hook so the server-side rebind
        // re-points the handle's terminal_id at the new pane; a pinned R6
        // hit must NOT (unchanged pinned semantics). Counter increments
        // pre-store-gate, so this stays network-silent (no store attached).
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let id = Uuid::new_v4().to_string();

        // Fresh pinned registration fires the hook once.
        reg.register_session(&id, "purpose", None).unwrap();
        assert_eq!(reg.handle_hook_fire_count(), 1);

        // Pinned re-register (R6 hit) does NOT refire.
        reg.register_session(&id, "purpose", None).unwrap();
        assert_eq!(reg.handle_hook_fire_count(), 1);

        // Sniffed R6 hit on the SAME session (operator hand-resumed the
        // pinned session) REFIRES the rebind.
        reg.register_sniffed_session(&id, "Terminal 2", None)
            .unwrap();
        assert_eq!(reg.handle_hook_fire_count(), 2);

        // Fresh sniffed registration of a different session fires once more.
        let other = Uuid::new_v4().to_string();
        reg.register_sniffed_session(&other, "Terminal 3", None)
            .unwrap();
        assert_eq!(reg.handle_hook_fire_count(), 3);

        // Sniffed re-sniff hit also refires (rebind is idempotent).
        reg.register_sniffed_session(&other, "Terminal 3", None)
            .unwrap();
        assert_eq!(reg.handle_hook_fire_count(), 4);
    }

    #[test]
    fn store_close_observer_closes_sniffed_coord_row() {
        // Review W2 end-to-end (minus Tauri): wire a lifecycle store's close
        // observer to close_session the way main.rs does, register a sniffed
        // session, then record_close the record — the coord row must get a
        // Closed outbox row and the index must evict.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let store_dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            crate::session::session_lifecycle_store::SessionLifecycleStore::open(
                store_dir.path().join("terminal-sessions.json"),
            )
            .unwrap(),
        );
        {
            // main.rs wires this through a Weak to avoid the Arc cycle; the
            // test uses a plain clone (registrar is cheap-to-clone shared
            // state, and the test scope owns both ends).
            let reg_for_obs = reg.clone();
            store.attach_close_observer(move |csid| reg_for_obs.close_session(csid));
        }

        let csid = Uuid::new_v4().to_string();
        store.record_open(
            crate::session::session_lifecycle_store::TerminalSessionRecord {
                claude_session_id: csid.clone(),
                config_dir: None,
                working_dir: Some("C:/repo".to_string()),
                page_id: "default".to_string(),
                zone_index: 0,
                title: Some("Terminal 1".to_string()),
                terminal_id: "term-1".to_string(),
                opened_at: 0,
                last_seen_at: 0,
                state: "open".to_string(),
                closed_at: None,
                close_reason: None,
                provider: crate::session::session_lifecycle_store::DEFAULT_PROVIDER.to_string(),
                origin: None,
                restore_pending_at: None,
                confirmed_at: None,
                handle: None,
                account_label: None,
                account_wrapper: None,
                session_name: None,
                name_source: None,
                tenant_id: None,
                task_run_id: None,
                bypass_permissions: None,
                restored_from_boot_at: None,
                restore_tier: None,
            },
        );
        let coord_id = reg
            .register_sniffed_session(&csid, "Terminal 1", None)
            .unwrap();

        store.record_close(&csid, "poll-dead");

        // Index evicted + a Closed row enqueued for the coord id.
        assert!(reg.session_id_for(&csid).is_none());
        let closed =
            reg.inner.outbox.pending().unwrap().into_iter().any(|r| {
                r.event_kind == SessionEventKind::Closed.as_str() && r.session_id == coord_id
            });
        assert!(closed, "record_close must propagate to a coord Closed row");

        // A repeat close is a no-op (observer only fires on a REAL
        // transition): still exactly one Closed row.
        store.record_close(&csid, "poll-dead");
        let closed_count = reg
            .inner
            .outbox
            .pending()
            .unwrap()
            .into_iter()
            .filter(|r| r.event_kind == SessionEventKind::Closed.as_str())
            .count();
        assert_eq!(closed_count, 1);
    }

    #[test]
    fn sniffed_close_emits_closed_row_and_evicts_index() {
        // close_session is keyed on the same claude_session_id keyspace, so
        // a sniffed registration can be closed by the same path.
        let _env = env_lock();
        std::env::remove_var("QONTINUI_SESSION_AUTOMATION_REGISTER");
        let (reg, _dir) = registrar();
        let csid = Uuid::new_v4().to_string();
        let coord_id = reg
            .register_sniffed_session(&csid, "Terminal 1", None)
            .unwrap();
        reg.close_session(&csid);
        assert!(reg.session_id_for(&csid).is_none());
        let closed =
            reg.inner.outbox.pending().unwrap().into_iter().any(|r| {
                r.event_kind == SessionEventKind::Closed.as_str() && r.session_id == coord_id
            });
        assert!(closed);
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

    /// A private emitter service whose coord base never resolves, so a flush
    /// returns before building a client or opening a socket. It refreshes its
    /// own leaked gauge rather than the process-wide one, so concurrent tests
    /// never read each other's queues.
    fn offline_service() -> (EmitterService, &'static std::sync::atomic::AtomicUsize) {
        fn no_coord() -> Option<String> {
            None
        }
        let gauge: &'static std::sync::atomic::AtomicUsize =
            Box::leak(Box::new(std::sync::atomic::AtomicUsize::new(0)));
        let service =
            EmitterService::spawn(no_coord, gauge).expect("the test box can spawn one thread");
        (service, gauge)
    }

    /// Poll a service's gauge until it reads `expected` or the wait runs out.
    /// The bound is two flush intervals: the service refreshes the gauge after
    /// every pass, and a close is acted on in the pass that receives it, so
    /// anything longer means the close was not honoured.
    fn wait_for_live_agents(gauge: &std::sync::atomic::AtomicUsize, expected: usize) -> bool {
        let deadline = std::time::Instant::now() + AGENT_LOG_FLUSH_INTERVAL * 2;
        let read = || gauge.load(std::sync::atomic::Ordering::Relaxed);
        while std::time::Instant::now() < deadline {
            if read() == expected {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        read() == expected
    }

    /// Threads of this process whose `comm` starts with `prefix` (Linux only:
    /// procfs is the one place a thread NAME is readable without a handle).
    /// The kernel truncates `comm` to 15 bytes, so the old per-session
    /// `agent-log-emit-<uuid>` threads read exactly `agent-log-emit-` and the
    /// service's `agent-log-emitter` reads `agent-log-emitt` — distinguishable.
    #[cfg(target_os = "linux")]
    fn threads_named(prefix: &str) -> Option<usize> {
        let tasks = std::fs::read_dir("/proc/self/task").ok()?;
        Some(
            tasks
                .flatten()
                .filter_map(|t| std::fs::read_to_string(t.path().join("comm")).ok())
                .filter(|comm| comm.trim_end().starts_with(prefix))
                .count(),
        )
    }

    /// Regression for the swallowed `Close` (plan
    /// `2026-08-28-runner-thread-and-socket-leak-wedges-the-accept-path`).
    ///
    /// `close()` sends `Entry(session_closed)` then `Close` back to back. The
    /// old per-session `drain_loop` received the entry, then coalesced the
    /// burst with `while let Ok(EmitMsg::Entry(e)) = rx.try_recv()` — which
    /// pulled the `Close` off the channel and, because the pattern did not
    /// match it, discarded it. With coord unreachable the entry was requeued
    /// forever and the thread, its client and its runtime lived until process
    /// exit. Against that loop this test would have timed out with the agent's
    /// queue still held; against the service the `Close(agent_id)` arm drops
    /// the queue in the same pass, whatever the final flush's outcome.
    #[test]
    fn close_after_emit_drops_the_agents_queue() {
        let (service, gauge) = offline_service();
        let agent = Uuid::new_v4();
        let handle = AgentLogEmitter::with_service(&service, agent, Some(Uuid::new_v4()), None);

        handle.emit("info", "stdout", Some(json!({ "text": "hello" })));
        assert!(
            wait_for_live_agents(gauge, 1),
            "the entry must open the agent's queue before the close is sent — \
             otherwise the assertion below would pass on the gauge's initial 0"
        );
        handle.close();

        assert!(
            wait_for_live_agents(gauge, 0),
            "the service still holds {} queue(s) two flush intervals after close()",
            gauge.load(std::sync::atomic::Ordering::Relaxed)
        );

        // A line after the close opens a fresh queue (coord is unreachable
        // here, so it is retained), and another close drops it again — the
        // close arm is not one-shot per agent.
        handle.emit("info", "stdout", Some(json!({ "text": "late" })));
        assert!(
            wait_for_live_agents(gauge, 1),
            "a post-close entry must open a queue"
        );
        handle.close();
        assert!(
            wait_for_live_agents(gauge, 0),
            "a second close must drop the fresh queue"
        );
    }

    /// Regression for the thread-per-session leak: 50 handles must add at
    /// most the service's own threads, never one per handle.
    ///
    /// On Linux the assertion is STRUCTURAL, read from `/proc/self/task/*/comm`:
    /// after 50 handles have emitted and closed, no thread named with the old
    /// per-session prefix `agent-log-emit-` exists at all. That cannot be
    /// false-failed by a neighbouring test spawning threads of its own, and it
    /// is red against the old design by construction (50 `agent-log-emit-<uuid>`
    /// threads). Elsewhere, where thread names are unreadable, the fallback is
    /// the process thread count within `baseline + 2` — `+1` for this test's
    /// private `agent-log-emitter` thread and one of headroom for the reqwest
    /// runtime the production service builds lazily (this offline service never
    /// does) — retried a bounded number of times because the reading is
    /// process-wide and a neighbour's short-lived thread can push one sample
    /// over.
    #[test]
    fn fifty_handles_add_no_thread_each() {
        #[cfg(target_os = "linux")]
        {
            let (service, gauge) = offline_service();
            let handles: Vec<AgentLogEmitter> = (0..50)
                .map(|_| AgentLogEmitter::with_service(&service, Uuid::new_v4(), None, None))
                .collect();
            for handle in &handles {
                handle.started("thread-count regression");
                handle.stream_line("one line");
                handle.close();
            }
            assert!(
                wait_for_live_agents(gauge, 0),
                "{} queue(s) still held after every handle closed",
                gauge.load(std::sync::atomic::Ordering::Relaxed)
            );
            let per_session =
                threads_named("agent-log-emit-").expect("/proc/self/task is readable on Linux");
            assert_eq!(
                per_session, 0,
                "{per_session} thread(s) carry the per-session prefix — the emitter is \
                 spawning per handle again"
            );
            assert!(
                threads_named("agent-log-emitt").is_some_and(|n| n >= 1),
                "the service thread itself must exist while the service is alive"
            );
            // Name-agnostic backstop: whatever a per-handle thread might be
            // called, 50 handles must not have produced anything like 50
            // emitter-family threads. Other tests hold a handful of offline
            // services at most.
            let family = threads_named("agent-log").expect("procfs readable");
            assert!(
                family < 50,
                "{family} agent-log* threads — one per handle again under a new name?"
            );
            drop(handles);
            drop(service);
        }
        #[cfg(not(target_os = "linux"))]
        {
            const ATTEMPTS: usize = 3;
            let mut readings: Vec<(usize, usize)> = Vec::new();
            for _ in 0..ATTEMPTS {
                let Some(baseline) = crate::health_monitor::thread_count_reading() else {
                    eprintln!("thread count is UNREADABLE on this platform — bound not asserted");
                    return;
                };
                let (service, gauge) = offline_service();
                let handles: Vec<AgentLogEmitter> = (0..50)
                    .map(|_| AgentLogEmitter::with_service(&service, Uuid::new_v4(), None, None))
                    .collect();
                for handle in &handles {
                    handle.started("thread-count regression");
                    handle.stream_line("one line");
                    handle.close();
                }
                assert!(
                    wait_for_live_agents(gauge, 0),
                    "{} queue(s) still held after every handle closed",
                    gauge.load(std::sync::atomic::Ordering::Relaxed)
                );
                let after = (0..10)
                    .filter_map(|_| {
                        std::thread::sleep(Duration::from_millis(20));
                        crate::health_monitor::thread_count_reading()
                    })
                    .min()
                    .expect("the thread count was readable a moment ago");
                if after <= baseline + 2 {
                    return;
                }
                readings.push((baseline, after));
                // Dropping `service` disconnects its channel and ends its
                // thread, so the next attempt starts from a clean slate.
                drop(handles);
                drop(service);
            }
            panic!(
                "thread count exceeded baseline + 2 on every attempt for 50 handles \
                 (baseline, after): {readings:?} — the emitter is spawning per handle again"
            );
        }
    }

    /// The service keys queues by the entry's own `agent_session_id`, caps
    /// each at `AGENT_LOG_QUEUE_CAP` dropping the OLDEST, and flags an agent
    /// for an immediate flush once its queue reaches a batch.
    #[test]
    fn absorb_queues_per_agent_caps_fifo_and_flags_full() {
        let mk = |agent: Uuid, n: usize| LogEntry {
            level: "info".to_string(),
            event: format!("e{n}"),
            payload: None,
            agent_session_id: Some(agent),
            device_id: None,
            tenant_id: None,
            occurred_at: None,
        };
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut queues = AgentQueues::new();
        let (mut full, mut closed) = (Vec::new(), Vec::new());

        for n in 0..AGENT_LOG_QUEUE_CAP + 3 {
            absorb(
                &mut queues,
                EmitMsg::Entry(mk(a, n)),
                &mut full,
                &mut closed,
            );
        }
        absorb(
            &mut queues,
            EmitMsg::Entry(mk(b, 0)),
            &mut full,
            &mut closed,
        );
        absorb(&mut queues, EmitMsg::Close(b), &mut full, &mut closed);

        assert_eq!(
            queues.len(),
            1,
            "the closed agent's queue is detached from the map at absorb time"
        );
        assert_eq!(queues[&a].len(), AGENT_LOG_QUEUE_CAP, "capped");
        assert_eq!(
            queues[&a].front().unwrap().event,
            "e3",
            "oldest dropped first"
        );
        assert_eq!(
            full,
            vec![a],
            "only the agent that reached a batch is flagged, once"
        );
        assert_eq!(closed.len(), 1, "one detached queue");
        assert_eq!(closed[0].0, b);
        assert_eq!(
            closed[0].1.len(),
            1,
            "it carries the entry sent before the close"
        );

        let mut dropped = Vec::new();
        absorb(
            &mut queues,
            EmitMsg::Entry(LogEntry {
                agent_session_id: None,
                ..mk(a, 0)
            }),
            &mut full,
            &mut dropped,
        );
        assert_eq!(queues.len(), 1, "an unstamped entry opens no queue");
    }

    /// The process-wide budget: over `AGENT_LOG_TOTAL_CAP`, the oldest entry
    /// of the LARGEST queue is dropped, and other queues are untouched.
    #[test]
    fn total_cap_drops_oldest_from_the_largest_queue() {
        let mk = |agent: Uuid, n: usize| LogEntry {
            level: "info".to_string(),
            event: format!("e{n}"),
            payload: None,
            agent_session_id: Some(agent),
            device_id: None,
            tenant_id: None,
            occurred_at: None,
        };
        let (big, mid, small) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let mut queues = AgentQueues::new();
        // big + mid + small = cap + 2, with big the largest by a margin.
        let big_len = AGENT_LOG_TOTAL_CAP / 2 + 2;
        let mid_len = AGENT_LOG_TOTAL_CAP / 4;
        let small_len = AGENT_LOG_TOTAL_CAP - big_len - mid_len + 2;
        queues.insert(big, (0..big_len).map(|n| mk(big, n)).collect());
        queues.insert(mid, (0..mid_len).map(|n| mk(mid, n)).collect());
        queues.insert(small, (0..small_len).map(|n| mk(small, n)).collect());
        assert!(
            big_len > mid_len && big_len > small_len,
            "fixture: big is largest"
        );

        enforce_total_cap(&mut queues);

        let total: usize = queues.values().map(std::collections::VecDeque::len).sum();
        assert_eq!(total, AGENT_LOG_TOTAL_CAP, "trimmed to exactly the budget");
        assert_eq!(
            queues[&big].len(),
            big_len - 2,
            "the largest queue lost the two"
        );
        assert_eq!(
            queues[&big].front().unwrap().event,
            "e2",
            "and it lost them from the FRONT (oldest first)"
        );
        assert_eq!(queues[&mid].len(), mid_len, "other queues untouched");
        assert_eq!(queues[&small].len(), small_len, "other queues untouched");
    }

    /// `requeue_front` restores a failed batch to the front in order and, over
    /// the per-agent cap, drops the OLDEST — the same policy as `absorb`. In
    /// the service the drain and the requeue happen back to back on one thread,
    /// so the trim is unreachable there; this guards the invariant for any
    /// caller that requeues into a queue that grew in between.
    #[test]
    fn requeue_front_restores_order_and_drops_oldest_on_overflow() {
        let mk = |n: usize| LogEntry {
            level: "info".to_string(),
            event: format!("e{n}"),
            payload: None,
            agent_session_id: None,
            device_id: None,
            tenant_id: None,
            occurred_at: None,
        };
        // A queue already at the cap, holding e3..; a failed batch e0,e1,e2
        // comes back to the front.
        let mut queue: std::collections::VecDeque<LogEntry> =
            (3..AGENT_LOG_QUEUE_CAP + 3).map(mk).collect();
        requeue_front(&mut queue, vec![mk(0), mk(1), mk(2)]);
        assert_eq!(queue.len(), AGENT_LOG_QUEUE_CAP, "trimmed back to the cap");
        assert_eq!(
            queue.front().unwrap().event,
            "e3",
            "the three OLDEST were dropped"
        );
        assert_eq!(
            queue.back().unwrap().event,
            format!("e{}", AGENT_LOG_QUEUE_CAP + 2),
            "the newest survive"
        );
    }

    /// A coord base that refuses the connection is a TRANSPORT failure: the
    /// batch is requeued intact and the caller is told to stop for the pass.
    /// The port is one a listener was bound to and then dropped, so the
    /// connect is refused immediately — no timeout is paid and no socket is
    /// left open.
    #[test]
    fn flush_batch_reports_transport_failure_and_requeues() {
        // `attach_device_auth_blocking` walks the real credential seam; keep it
        // off the host's secure storage and keychain like the other
        // env-touching tests in this module.
        let _env = env_lock();
        // Restore both on exit (value or absence): CI runs the whole binary
        // with QONTINUI_DISABLE_KEYCHAIN=1, so a bare remove_var would unset
        // CI's default for every later test, and a storage dir left pointing
        // at this tempdir would outlive it.
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            "QONTINUI_SECURE_STORAGE_DIR",
            "QONTINUI_DISABLE_KEYCHAIN",
        ]);
        let storage = tempfile::tempdir().expect("tempdir");
        std::env::set_var("QONTINUI_SECURE_STORAGE_DIR", storage.path());
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        // The port was bound and released a moment ago; a sibling test binding
        // 127.0.0.1:0 could in principle land on it before the connect, which
        // would answer instead of refusing. One in the ephemeral range per
        // concurrent bind — noted, not guarded.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
            l.local_addr().unwrap().port()
        };
        let base = format!("http://127.0.0.1:{port}");
        let agent = Uuid::new_v4();
        let mut queue: std::collections::VecDeque<LogEntry> = (0..3)
            .map(|n| LogEntry {
                level: "info".to_string(),
                event: format!("e{n}"),
                payload: None,
                agent_session_id: Some(agent),
                device_id: None,
                tenant_id: None,
                occurred_at: None,
            })
            .collect();
        let mut client = None;

        assert_eq!(
            flush_batch(&mut client, Some(&base), agent, &mut queue),
            Flush::TransportFailed
        );
        assert_eq!(queue.len(), 3, "requeued intact");
        assert_eq!(queue.front().unwrap().event, "e0", "in original order");
        assert!(
            client.is_some(),
            "the client was built on this (the caller's) thread"
        );

        let mut empty = std::collections::VecDeque::new();
        assert_eq!(
            flush_batch(&mut client, Some(&base), agent, &mut empty),
            Flush::Skipped,
            "nothing to send is Skipped, never a transport verdict"
        );
        assert_eq!(
            flush_batch(&mut client, None, agent, &mut queue),
            Flush::Skipped,
            "no coord base is Skipped"
        );
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
