//! Coord-mcp `.mcp.json` provisioning — the leaf module owning every helper that
//! writes a coord device/agent JWT into a session's `.mcp.json`.
//!
//! Extracted out of `agent_runtime.rs` so the terminal chokepoint
//! (`agent_worktree::isolated_edit::acquire_for_terminal`) can call provisioning
//! WITHOUT creating a circular dependency: `agent_runtime` already depends on
//! `agent_worktree`, so `agent_worktree → agent_runtime` would close a cycle.
//! This module depends only on `crate::auth`, `std::fs`, `serde_json`, `base64`
//! (plus read-only port/handle lookups) — a leaf both callers can share.
//!
//! # Live-token loopback proxy (plan 2026-06-09-coord-mcp-live-token-proxy)
//!
//! Coord device JWTs have a ~4h TTL and Claude Code's MCP client reads
//! `.mcp.json` exactly once at connect — a static baked bearer dies with the
//! snapshot, and re-stamping the file does nothing (the client never re-reads).
//! DEVICE-provisioned sessions therefore get a config pointing at the runner's
//! own loopback `POST /coord-mcp` route, which injects a freshly-read device
//! JWT per request (see `mcp_api::coord_mcp_proxy_handler`). Authentication to
//! the proxy is a per-session nonce ([`COORD_MCP_PROXY_KEY_HEADER`]) held in an
//! in-memory map that is ALSO mirrored to the encrypted local store
//! (plan 2026-06-13 Phase 3b, gated by `COORD_MCP_PERSIST_NONCES`, default ON)
//! so an already-written `.mcp.json` keeps validating across a runner
//! rebuild/restart — the MCP client never re-reads the file, so a process-only
//! nonce would 401 every live agent after a routine restart.
//!
//! SCOPE-ELEVATION TRAP: agent-spawn sessions deliberately carry a narrower
//! coord-minted `SubType::Agent` JWT. The DEVICE proxy attaches the live DEVICE
//! JWT, so an agent session must NEVER route through the device proxy. Instead,
//! agent sessions get their OWN per-agent proxy ([`ProxyPrincipal::Agent`]) that
//! injects THEIR OWN refreshed agent JWT — held in [`AGENT_TOKENS`], a
//! process-global `SharedToken` slot refreshed proactively from the agent's
//! heartbeat loop so the 4h TTL never expires for a live agent. The proxy gate
//! structurally binds nonce→principal→`sub_type`: a device nonce can only ever
//! forward a `sub_type == "device"` bearer, and an agent nonce a
//! `sub_type == "agent"` bearer — neither can elevate into the other.
//!
//! # Multi-tenant disposition (Phase 8b, plan
//! `2026-07-02-session-scoped-multi-tenant-device-binding` §D4)
//!
//! The proxy is SESSION-SCOPED where it matters and default-scoped where
//! that is the honest binding:
//!
//! - **Agent principal — session-scoped by construction.** A coord-spawned
//!   agent session's bearer is its own coord-minted agent JWT, whose
//!   `tenant_id` claim is the SESSION's binding frozen at mint (coord
//!   Phase 4 validates membership at spawn). Two concurrent agent sessions
//!   for two tenants each present their own tenant's claim with zero
//!   runner-side selection logic.
//! - **Device principal — DEFAULT binding by construction.** Device-path
//!   nonces serve operator terminals and gate-continuation shells, which
//!   run under the device's default binding (`machine.json::
//!   active_tenant_id`, default-for-new-sessions); the injected live device
//!   JWT is the legacy slot = the default binding's credential. A future
//!   spawn path that provisions a device-proxied session under a
//!   NON-default binding must select via `auth::device_bearer_for` rather
//!   than widen this path silently.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tracing::{info, warn};
use uuid::Uuid;

/// Which identity a registered proxy nonce is bound to. The nonce→principal
/// binding is the structural backstop against the scope-elevation trap: the
/// pre-forward gate ([`proxy_request_gate`]) requires the injected bearer's
/// `sub_type` claim to match the principal, so a device nonce can never forward
/// an agent token (or vice-versa).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ProxyPrincipal {
    /// The runner's own device identity — the bearer is the live device JWT read
    /// from `AuthManager` per request.
    Device,
    /// A specific spawned agent — the bearer is THAT agent's own refreshed JWT,
    /// looked up from [`AGENT_TOKENS`] by `agent_id` per request.
    Agent { agent_id: Uuid },
}

impl ProxyPrincipal {
    /// The `sub_type` claim value a forwarded bearer must carry for this
    /// principal. The gate rejects any bearer whose `sub_type` differs.
    fn expected_sub_type(&self) -> &'static str {
        match self {
            ProxyPrincipal::Device => "device",
            ProxyPrincipal::Agent { .. } => "agent",
        }
    }
}

/// What a registered proxy nonce maps to: the session workdir it was provisioned
/// into PLUS the identity ([`ProxyPrincipal`]) whose bearer the proxy may inject
/// for it.
#[derive(Clone, Debug)]
struct NonceBinding {
    workdir: String,
    principal: ProxyPrincipal,
}

/// Header carrying the per-session loopback nonce that authenticates a
/// session's MCP client to the runner-local `/coord-mcp` proxy route.
/// Lowercase — HTTP header names are case-insensitive and axum's `HeaderMap`
/// keys are lowercased; the `.mcp.json` writer emits the canonical-case form.
pub(crate) const COORD_MCP_PROXY_KEY_HEADER: &str = "x-coord-mcp-proxy-key";

/// Resolve the coord HTTP base: `COORD_HTTP_URL` env first, then the active
/// profile's `coord_url` (ws→http normalized). `None` when neither is set.
/// Inline copy of `agent_runtime::coord_http_base` — kept local so this leaf
/// module does NOT depend on `agent_runtime` (which depends back on us).
fn coord_http_base() -> Option<String> {
    // Delegates to the shared resolver. `None` when nothing is configured; the
    // localhost fallback for the `.mcp.json` write is applied downstream by
    // `coord_base_url`, unchanged.
    match qontinui_runner_lib::profiles::resolve_coord_base() {
        qontinui_runner_lib::profiles::CoordBase::Configured(base) => Some(base),
        _ => None,
    }
}

/// The coord HTTP base (no path, no trailing slash): `COORD_HTTP_URL` env →
/// active profile's `coord_url` → localhost fallback. Shared by every loopback
/// proxy forwarder (the `/mcp` JSON-RPC passthrough AND the nonce-gated claims
/// read passthrough in `mcp_api`) so they all resolve the coord base
/// identically — a proxy route must never re-derive it from env alone.
pub(crate) fn coord_base_url() -> String {
    let coord_url = std::env::var("COORD_HTTP_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(coord_http_base)
        .unwrap_or_else(|| "http://localhost:9870".to_string());
    coord_url.trim_end_matches('/').to_string()
}

/// The full coord `/mcp` endpoint URL: [`coord_base_url`] with `/mcp` appended.
/// Shared by the static-bearer `.mcp.json` writer (agent path) and the loopback
/// proxy forwarder (`mcp_api::coord_mcp_proxy_handler`).
pub(crate) fn coord_mcp_url() -> String {
    format!("{}/mcp", coord_base_url())
}

/// In-memory nonce registry for the loopback `/coord-mcp` proxy:
/// nonce → [`NonceBinding`] (the workdir it was provisioned into + the
/// [`ProxyPrincipal`] whose bearer it may inject). DEVICE bindings are mirrored
/// to the encrypted local store (Phase 3b) when `COORD_MCP_PERSIST_NONCES` is
/// not `0`, so the device set survives a runner rebuild/restart and an
/// already-written `.mcp.json` keeps validating (the MCP client never re-reads
/// the file). AGENT bindings are NEVER persisted (OQ3): a restarted runner has
/// no live agent session, so a restored agent nonce MUST hard-fail closed — the
/// handler 401s on the absent [`AGENT_TOKENS`] slot (process-global, never
/// persisted). With persistence disabled this degrades to the prior
/// process-lifetime-only behavior for device nonces too.
static PROXY_NONCES: OnceLock<Mutex<HashMap<String, NonceBinding>>> = OnceLock::new();

/// Per-agent live-token registry for the agent-proxy path: `agent_id` →
/// the agent's [`crate::agent_token::SharedToken`] slot. Built fresh at the
/// agent spawn site from the launch payload's JWT (there is no other live owner
/// — `agent_daemons::spawn_for_agent` has no production caller), refreshed
/// proactively from the agent's heartbeat loop, and dropped on teardown. A nonce
/// bound to [`ProxyPrincipal::Agent`] whose `agent_id` has no slot here is a
/// hard 401 — which is exactly what makes a restart (or a torn-down agent)
/// fail closed.
static AGENT_TOKENS: OnceLock<Mutex<HashMap<Uuid, crate::agent_token::SharedToken>>> =
    OnceLock::new();

fn agent_tokens() -> &'static Mutex<HashMap<Uuid, crate::agent_token::SharedToken>> {
    AGENT_TOKENS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register (or replace) the live-token slot for `agent_id`. Called at the agent
/// spawn site after building the slot from the launch payload's JWT.
pub(crate) fn register_agent_token(agent_id: Uuid, slot: crate::agent_token::SharedToken) {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .insert(agent_id, slot);
}

/// Look up the live-token slot for `agent_id` (clones the `Arc` out so the lock
/// is released immediately). `None` after teardown / before registration → the
/// proxy handler 401s, failing closed.
pub(crate) fn lookup_agent_token(agent_id: Uuid) -> Option<crate::agent_token::SharedToken> {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .get(&agent_id)
        .cloned()
}

/// Drop the live-token slot for `agent_id` on teardown so a torn-down agent's
/// nonce hard-fails closed. Idempotent.
pub(crate) fn remove_agent_token(agent_id: Uuid) {
    agent_tokens()
        .lock()
        .expect("agent token map poisoned")
        .remove(&agent_id);
}

/// Guards [`restore_proxy_nonces_from_store`] so a second boot-restore (e.g. an
/// idempotent auto-start re-invocation) never re-loads over live in-memory
/// nonces minted since the first restore.
static PROXY_NONCES_RESTORED: OnceLock<()> = OnceLock::new();

fn proxy_nonces() -> &'static Mutex<HashMap<String, NonceBinding>> {
    PROXY_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Grace window (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart,
/// Change 3, defense in depth): a DEVICE nonce evicted by a same-workdir re-mint
/// stays valid for this long so an in-flight MCP client that cached it rides
/// through until it reconnects and re-reads the freshly-written `.mcp.json` (the
/// client never re-reads the file mid-connection, so a hard eviction 401s it the
/// instant the file is rewritten). Bounded — the accept-set widening lasts only
/// this window, and only for a device nonce the runner itself just superseded.
/// AGENT nonces are NEVER graced: they must hard-fail closed on re-mint/restart
/// (the scope-elevation non-goal, OQ3).
const NONCE_GRACE_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// A device nonce kept transiently valid after eviction, with its expiry.
struct GracedNonce {
    expires_at: std::time::Instant,
}

/// Transient grace registry: an evicted DEVICE nonce → its expiry. Separate from
/// [`PROXY_NONCES`] so the live map stays the single source of truth for a
/// currently-provisioned nonce and grace never reaches disk (it is process-local
/// and intentionally forgotten across a restart — Change 1's adopt-on-disk path
/// owns cross-restart continuity).
static GRACED_NONCES: OnceLock<Mutex<HashMap<String, GracedNonce>>> = OnceLock::new();

fn graced_nonces() -> &'static Mutex<HashMap<String, GracedNonce>> {
    GRACED_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Move DEVICE nonces just evicted for `_workdir` into the grace registry with a
/// [`NONCE_GRACE_TTL`] expiry, opportunistically pruning expired entries so the
/// map stays bounded. Only device nonces are passed here (the caller filters);
/// agent nonces are dropped outright to fail closed.
fn grace_evicted_device_nonces(nonces: &[String]) {
    if nonces.is_empty() {
        return;
    }
    let now = std::time::Instant::now();
    let expires_at = now + NONCE_GRACE_TTL;
    let mut graced = graced_nonces().lock().expect("graced nonce map poisoned");
    graced.retain(|_, g| g.expires_at > now);
    for n in nonces {
        graced.insert(n.clone(), GracedNonce { expires_at });
    }
}

/// True iff `nonce` is a DEVICE nonce still inside its grace TTL (Change 3).
/// Lazily evicts it once expired so grace fails closed exactly at the deadline.
fn graced_nonce_is_valid(nonce: &str) -> bool {
    let now = std::time::Instant::now();
    let mut graced = graced_nonces().lock().expect("graced nonce map poisoned");
    match graced.get(nonce) {
        Some(g) if g.expires_at > now => true,
        Some(_) => {
            graced.remove(nonce);
            false
        }
        None => false,
    }
}

/// Project the live nonce map down to the DEVICE-only `nonce → workdir` shape
/// the encrypted store persists (OQ3): agent bindings are dropped so they never
/// reach disk. The store contract is unchanged (`HashMap<String, String>`), so
/// the persistence/restore seams and their tests stay green.
fn device_nonce_snapshot(map: &HashMap<String, NonceBinding>) -> HashMap<String, String> {
    map.iter()
        .filter(|(_, b)| b.principal == ProxyPrincipal::Device)
        .map(|(n, b)| (n.clone(), b.workdir.clone()))
        .collect()
}

/// True unless `COORD_MCP_PERSIST_NONCES` is explicitly set to `0` — persistence
/// is ON by default (resolved in the plan: the nonce is a loopback-only key, so
/// persisting it at rest is acceptable and is the only thing that keeps an
/// already-running agent alive across a restart). `=0` reverts to the prior
/// in-memory-only behavior.
fn nonce_persistence_enabled() -> bool {
    // In production: ON unless explicitly `=0`. In test builds: OFF unless
    // explicitly enabled — so the many nonce-minting unit tests never touch the
    // developer's real encrypted store; the persistence tests opt IN by setting
    // the env (under a serializing lock) to a temp store dir.
    match std::env::var("COORD_MCP_PERSIST_NONCES") {
        Ok(v) => v.trim() != "0",
        Err(_) => !cfg!(test),
    }
}

/// Mirror the live in-memory nonce map into the encrypted local store. No-op
/// when persistence is disabled. Best-effort: a store failure only `warn!`s —
/// the in-memory map stays authoritative for this process.
///
/// Opens the DEFAULT [`SecureStorage`] (honoring `QONTINUI_SECURE_STORAGE_DIR`)
/// and delegates to [`persist_proxy_nonces_with_store`]. Tests inject their own
/// store via the `_with_store` seam so they never touch the developer's real
/// encrypted store NOR the process-global env (which would race sibling tests
/// that read the default store, e.g. `auth::device_jwt_tests`).
fn persist_proxy_nonces(map: &HashMap<String, NonceBinding>) {
    if !nonce_persistence_enabled() {
        return;
    }
    match crate::secure_storage::SecureStorage::new() {
        Ok(store) => persist_proxy_nonces_with_store(&store, map),
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, proxy nonces not persisted: {e}");
        }
    }
}

/// Mirror `map` into the GIVEN store. The store is injected so the persistence
/// path is unit-testable against a temp-dir [`SecureStorage::with_path`] without
/// mutating `QONTINUI_SECURE_STORAGE_DIR` (which is process-global and pollutes
/// every other test that reads the default store). The `nonce_persistence_enabled`
/// gate is the CALLER's concern — handing a store IS the decision to persist.
fn persist_proxy_nonces_with_store(
    store: &crate::secure_storage::SecureStorage,
    map: &HashMap<String, NonceBinding>,
) {
    // OQ3: persist DEVICE bindings only — agent nonces must never reach disk.
    let device_only = device_nonce_snapshot(map);
    if let Err(e) = store.store_coord_mcp_nonces(&device_only) {
        warn!("coord_mcp: failed to persist proxy nonces: {e}");
    }
}

/// Restore persisted proxy nonces into the in-memory registry on boot (Phase
/// 3b). Idempotent + run-once: merges the persisted set UNDER any nonces already
/// minted this process (live mints win on key collision, which cannot happen in
/// practice — the persisted set predates this process). No-op when persistence
/// is disabled. Wire this into the same startup path as the other auto-start
/// tasks so already-written `.mcp.json` nonces keep validating post-restart.
/// Returns the number of nonces in the live registry after the restore merge
/// (0 when persistence is disabled, storage is unavailable, or nothing was
/// persisted). The count is surfaced by the boot task (plan 2026-07-07 Change 2
/// observability) so a future silent rotation — restore brought back 0 then
/// self-heal had to mint fresh — is visible in the logs.
pub(crate) fn restore_proxy_nonces_from_store() -> usize {
    if !nonce_persistence_enabled() {
        return 0;
    }
    if PROXY_NONCES_RESTORED.set(()).is_err() {
        // Already restored once this process — report the current live size so a
        // duplicate boot-task run still logs a coherent count.
        return proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .len();
    }
    let store = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s,
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, cannot restore proxy nonces: {e}");
            return 0;
        }
    };
    restore_proxy_nonces_from(&store)
}

/// Merge the persisted nonce set from the GIVEN store into the live in-memory
/// registry (live mints win on collision). The store is injected so the
/// restore path is unit-testable against a temp-dir store without the
/// run-once `PROXY_NONCES_RESTORED` guard or any global-env mutation. Returns
/// the live map size after the merge.
fn restore_proxy_nonces_from(store: &crate::secure_storage::SecureStorage) -> usize {
    let persisted = store.load_coord_mcp_nonces();
    if persisted.is_empty() {
        return proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .len();
    }
    let restored = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        for (nonce, workdir) in persisted {
            // Only DEVICE bindings are ever persisted (OQ3), so a restored entry
            // is unconditionally a Device principal. An agent nonce can never be
            // restored — its slot is process-global and gone after a restart, so
            // it would hard-fail closed anyway.
            map.entry(nonce).or_insert(NonceBinding {
                workdir,
                principal: ProxyPrincipal::Device,
            });
        }
        map.len()
    };
    info!("coord_mcp: restored {restored} persisted proxy nonce(s) from secure storage");
    restored
}

/// Mint + register a fresh per-session proxy nonce for `workdir`, returning it.
/// Any prior nonce registered for the same workdir is evicted — a re-provision
/// rewrites `.mcp.json`, so the old nonce is unreachable and keeping it would
/// only widen the accept set. The updated set is mirrored to the encrypted
/// store (Phase 3b) so it survives a restart.
fn register_proxy_nonce(workdir: &str) -> String {
    let (nonce, snapshot) = mint_and_register_nonce(workdir, ProxyPrincipal::Device);
    persist_proxy_nonces(&snapshot);
    nonce
}

/// Mint + register a fresh per-session proxy nonce bound to a specific AGENT for
/// `workdir`. Unlike [`register_proxy_nonce`] this is NOT persisted (OQ3) — an
/// agent nonce must hard-fail closed across a restart, which is automatic since
/// [`persist_proxy_nonces`] drops non-device bindings. The per-request bearer
/// comes from the agent's own [`AGENT_TOKENS`] slot, never the device JWT.
pub(crate) fn register_agent_proxy_nonce(workdir: &str, agent_id: Uuid) -> String {
    let (nonce, snapshot) = mint_and_register_nonce(workdir, ProxyPrincipal::Agent { agent_id });
    // Mirror to the store as a no-op for the agent entry (device entries in the
    // same snapshot, if any, are still persisted) — `persist_proxy_nonces`
    // filters agent bindings out, so this never writes the agent nonce to disk.
    persist_proxy_nonces(&snapshot);
    nonce
}

/// Mint a fresh nonce, evict any prior nonce for `workdir`, insert it, and
/// return `(nonce, snapshot)` — WITHOUT persisting. Split from the persistence
/// step so a test can mint and then mirror to an INJECTED store
/// ([`persist_proxy_nonces_with_store`]) instead of the default store reached
/// via the process-global `QONTINUI_SECURE_STORAGE_DIR`.
fn mint_and_register_nonce(
    workdir: &str,
    principal: ProxyPrincipal,
) -> (String, HashMap<String, NonceBinding>) {
    // Two v4 UUIDs (~244 bits of randomness) — v4, NOT v7: the v7 prefix is a
    // timestamp, which would gut the entropy this nonce exists to provide.
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let snapshot = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        // Change 3: collect the DEVICE nonces being evicted for this workdir so
        // they ride a short grace TTL — an in-flight client that cached one keeps
        // validating until it reconnects. Agent nonces are NOT graced (they must
        // hard-fail closed on re-mint), so they are simply dropped by `retain`.
        let evicted_device: Vec<String> = map
            .iter()
            .filter(|(n, b)| {
                b.workdir == workdir && b.principal == ProxyPrincipal::Device && *n != &nonce
            })
            .map(|(n, _)| n.clone())
            .collect();
        map.retain(|_, b| b.workdir != workdir);
        map.insert(
            nonce.clone(),
            NonceBinding {
                workdir: workdir.to_string(),
                principal,
            },
        );
        grace_evicted_device_nonces(&evicted_device);
        map.clone()
    };
    (nonce, snapshot)
}

/// Request header the runner proxy injects on forwarded `coord_*` calls: the
/// calling terminal's coord `agent_session_id`, so coord self-identifies the
/// caller deterministically instead of guessing the device's most-recent
/// session (session-fabric Phase 0). MUST match coord's `CALLER_SESSION_HEADER`.
pub(crate) const CALLER_SESSION_HEADER: &str = "x-coord-caller-session";

/// `true` when deterministic caller self-identification is enabled
/// (`COORD_SESSION_SELF_ID=observe`). Off/unset/any-other value ⇒ the proxy
/// injects nothing and coord keeps its fuzzy fallback — byte-for-byte today's
/// behavior. Mirrors coord's own gate so both halves arm from one env var.
pub(crate) fn session_self_id_enabled() -> bool {
    std::env::var("COORD_SESSION_SELF_ID").as_deref() == Ok("observe")
}

/// The session WORKDIR a registered proxy nonce was provisioned into (the
/// terminal's cwd / isolated worktree path). `None` for an empty or
/// unregistered nonce. Backs session-fabric Phase 0 caller self-identification:
/// the proxy maps nonce → workdir → task_run_id → coord `agent_session_id`.
pub(crate) fn workdir_for_nonce(nonce: &str) -> Option<String> {
    if nonce.is_empty() {
        return None;
    }
    proxy_nonces()
        .lock()
        .expect("proxy nonce map poisoned")
        .get(nonce)
        .map(|b| b.workdir.clone())
}

/// True iff `nonce` is a currently-registered per-session proxy key OR a DEVICE
/// nonce still inside its post-eviction grace TTL (Change 3). The live-map lock
/// is taken and released before the grace check so the two maps are never held
/// at once.
pub(crate) fn proxy_nonce_is_valid(nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    let in_live = proxy_nonces()
        .lock()
        .expect("proxy nonce map poisoned")
        .contains_key(nonce);
    in_live || graced_nonce_is_valid(nonce)
}

/// Resolve the [`ProxyPrincipal`] a registered nonce is bound to. `None` for an
/// empty or unregistered nonce — the handler treats that as a 401. The handler
/// resolves the principal BEFORE reading any bearer, so the bearer it injects is
/// chosen by the binding (device JWT vs the agent's own JWT) rather than the
/// other way around.
pub(crate) fn proxy_principal_for_nonce(nonce: &str) -> Option<ProxyPrincipal> {
    if nonce.is_empty() {
        return None;
    }
    let live = proxy_nonces()
        .lock()
        .expect("proxy nonce map poisoned")
        .get(nonce)
        .map(|b| b.principal.clone());
    // Grace fallback (Change 3): only DEVICE nonces are ever graced, so a graced
    // hit resolves to a Device principal — the handler then injects the live
    // device JWT and `proxy_request_gate` still enforces device-nonce ⇒
    // device-bearer (no scope-elevation surface).
    live.or_else(|| graced_nonce_is_valid(nonce).then_some(ProxyPrincipal::Device))
}

/// Pre-forward gate for the loopback `/coord-mcp` proxy route. Pure over its
/// inputs (no I/O) so the 401 paths are unit-testable without a live coord:
///
/// 1. `nonce` must be a registered per-session key ([`proxy_nonce_is_valid`])
///    — stops other local processes borrowing the runner's coord identity.
/// 2. `bearer` (the resolved per-request token — the live device JWT for a
///    [`ProxyPrincipal::Device`] nonce, the agent's own refreshed JWT for an
///    [`ProxyPrincipal::Agent`] nonce) must be present and decode a `sub_type`
///    matching the bound `principal` ([`ProxyPrincipal::expected_sub_type`]).
///
/// The `nonce → principal → sub_type` chain is the structural backstop against
/// the scope-elevation trap: a device nonce can only ever forward a device
/// token and an agent nonce only ever an agent token — neither can elevate into
/// the other. The handler resolves `principal` from the nonce BEFORE picking the
/// bearer, so this gate only re-validates that pairing.
///
/// `Ok(())` means: forward with `Authorization: Bearer <bearer>`.
pub(crate) fn proxy_request_gate(
    nonce: Option<&str>,
    bearer: Option<&str>,
    principal: &ProxyPrincipal,
) -> Result<(), (u16, String)> {
    if !proxy_nonce_is_valid(nonce.unwrap_or("")) {
        return Err((
            401,
            "missing or unrecognized X-Coord-Mcp-Proxy-Key".to_string(),
        ));
    }
    let bearer = match bearer {
        Some(b) if !b.trim().is_empty() => b,
        _ => {
            return Err((
                401,
                "no live JWT available for this proxy session".to_string(),
            ));
        }
    };
    let expected = principal.expected_sub_type();
    match jwt_unverified_claim(bearer, "sub_type").as_deref() {
        Some(st) if st == expected => Ok(()),
        other => Err((
            401,
            format!(
                "proxy bearer sub_type={other:?} does not match the nonce's \
                 bound principal (expected {expected:?}) — refusing to forward"
            ),
        )),
    }
}

/// Phase 3 (terminal-autonomy-survives-logout): maximum TOTAL time the
/// DEVICE-path coord-mcp proxy will wait for the device-JWT refresher to
/// re-mint a momentarily-missing token before it degrades to an actionable
/// retry error. Bounded tightly — a proxy request must NEVER block
/// indefinitely on a credential gap. The device JWT re-mints from the
/// preserved Cognito session in seconds (Phase 1), so this smooths the common
/// re-mint-window / transient-backoff gap (Phases 1-2) without hanging.
pub(crate) const DEVICE_JWT_REMINT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
/// Poll interval while waiting for the [`DEVICE_JWT_REMINT_WAIT`] re-mint.
pub(crate) const DEVICE_JWT_REMINT_POLL: std::time::Duration =
    std::time::Duration::from_millis(250);

/// The actionable, retry-shaped error the DEVICE-path proxy returns when the
/// device JWT is STILL missing after the bounded [`DEVICE_JWT_REMINT_WAIT`].
///
/// Distinct from the gate's hard 401s (bad/absent nonce, scope-elevation): this
/// is a TRANSIENT credential gap, not an auth failure — the autonomous session
/// is alive and the refresher is re-minting, so the caller should simply retry.
/// `503` (the canonical "temporarily unavailable, retry" code) keeps it an
/// error the MCP client will not mistake for success while signalling
/// retry-ability; the message is human/agent-actionable rather than a bare
/// "no live JWT available". The nonce/scope FAIL-CLOSED 401s in
/// [`proxy_request_gate`] are unchanged — only the missing-device-JWT case
/// degrades to this.
pub(crate) fn device_jwt_refreshing_error() -> (u16, String) {
    (
        503,
        "coord credential refreshing — autonomous session will resume; \
         retry shortly"
            .to_string(),
    )
}

/// Bounded poll for a usable token: returns as soon as `read_usable` yields a
/// non-empty token, or `None` once `total` elapses. Generic over the reader so
/// the bound/termination behavior is unit-testable without a live AuthManager
/// or a real 5s wait. NEVER blocks indefinitely — the deadline is checked every
/// `interval`.
async fn await_remint_with<F, Fut>(
    mut read_usable: F,
    total: std::time::Duration,
    interval: std::time::Duration,
) -> Option<String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let deadline = tokio::time::Instant::now() + total;
    loop {
        if let Some(t) = read_usable().await {
            if !t.trim().is_empty() {
                return Some(t);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(interval).await;
    }
}

/// Read the device JWT ONLY if it is freshly usable (present, non-empty, and
/// not stale per [`AuthManager::device_jwt_needs_refresh`]). Filesystem I/O, so
/// it runs on a blocking thread off the async executor — the same discipline
/// the proxy handler's first read uses. Used by [`await_device_jwt_remint`] to
/// detect "a usable JWT is now present" after a refresher kick.
async fn read_usable_device_jwt() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let am = crate::auth::AuthManager::new();
        match am.device_jwt_needs_refresh() {
            Ok(false) => am.get_access_token().ok().filter(|t| !t.trim().is_empty()),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
}

/// Phase 3 graceful-degrade for the DEVICE proxy path: when the device JWT is
/// momentarily absent, KICK the refresher (so it re-mints immediately from the
/// preserved Cognito session instead of waiting out its sleep) and wait a
/// tightly-bounded [`DEVICE_JWT_REMINT_WAIT`] for a usable JWT to appear.
/// Returns the fresh token if one re-mints within the bound, else `None` (the
/// caller then degrades to [`device_jwt_refreshing_error`]). Agent-bound proxy
/// requests do NOT use this — they refresh via their own `AGENT_TOKENS` slot.
pub(crate) async fn await_device_jwt_remint() -> Option<String> {
    crate::mcp::device_jwt_refresher::commands::kick_device_jwt_refresher().await;
    await_remint_with(
        read_usable_device_jwt,
        DEVICE_JWT_REMINT_WAIT,
        DEVICE_JWT_REMINT_POLL,
    )
    .await
}

/// Resolve the runner's ACTUALLY-BOUND local API port for loopback URLs:
/// the managed `AppState.api_port` (set at bind time) via the process-global
/// `AppHandle`. Returns `None` — fail-closed, Phase 3a — when no Tauri runtime
/// / managed `AppState` is reachable, rather than degrading to the bootstrap
/// [`crate::mcp::types::get_mcp_api_port`] default (`MCP_API_PORT=9876`). That
/// default is correct only by luck on a single-runner box and silently WRONG on
/// any secondary/temp runner bound to a different port (the F1 root cause: a
/// `:9876` URL written into a config whose live proxy is on `:9877`). Callers
/// MUST treat `None` as "refuse to write a proxy config" — a dead config that
/// looks valid is worse than an absent one.
pub(crate) fn resolve_bound_api_port() -> Option<u16> {
    if let Some(app) = crate::tauri_app_handle::current() {
        use tauri::Manager;
        if let Some(state) = app.try_state::<std::sync::Arc<crate::commands::AppState>>() {
            return Some(crate::mcp::types::runner_api_port(state.inner()));
        }
    }
    None
}

/// Write the DEVICE-path `.mcp.json`: an `http`-transport server pointing at
/// the runner's own loopback `/coord-mcp` proxy on the ACTUALLY-BOUND API
/// port, authenticated by a freshly-minted per-session nonce — and NO baked
/// bearer. The proxy injects a live device JWT per request, so the config
/// survives the 4h token TTL that kills static-bearer configs in sessions
/// that outlive their snapshot (the MCP client never re-reads `.mcp.json`).
pub(crate) fn write_coord_mcp_proxy_config(primary_wt: &str, bound_port: u16) {
    let nonce = register_proxy_nonce(primary_wt);
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{bound_port}/coord-mcp"),
                "headers": {
                    "X-Coord-Mcp-Proxy-Key": nonce,
                }
            }
        }
    });
    write_mcp_json(primary_wt, &mcp_config);
}

/// Write the AGENT-path `.mcp.json`: identical shape to
/// [`write_coord_mcp_proxy_config`] (loopback proxy URL on the bound port + a
/// per-session nonce header, NO baked bearer), but the nonce is bound to
/// [`ProxyPrincipal::Agent`] for `agent_id` — so the proxy injects THAT agent's
/// own refreshed JWT (from [`AGENT_TOKENS`]) per request, never the device JWT.
/// This is what lets an agent session outlive the 4h agent-JWT TTL (the static
/// bake at the old spawn site silently lost coord-mcp at expiry).
///
/// The caller MUST register the agent's [`crate::agent_token::SharedToken`] via
/// [`register_agent_token`] (and drive proactive refresh) — an agent nonce with
/// no live slot hard-fails closed (401).
pub(crate) fn write_coord_mcp_agent_proxy_config(
    primary_wt: &str,
    bound_port: u16,
    agent_id: Uuid,
) {
    let nonce = register_agent_proxy_nonce(primary_wt, agent_id);
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{bound_port}/coord-mcp"),
                "headers": {
                    "X-Coord-Mcp-Proxy-Key": nonce,
                }
            }
        }
    });
    write_mcp_json(primary_wt, &mcp_config);
}

/// Filename of the Phase-1a degraded-only breadcrumb dropped into a session
/// workdir when coord-mcp provisioning is degraded (no JWT, unresolvable port,
/// or a failed reachability probe). Referenced by the `/gate` skill + CLAUDE.md.
pub(crate) const COORD_MCP_STATUS_FILE: &str = ".coord-mcp-status";

/// Env var the runner injects at spawn carrying the absolute path of a
/// runner-owned coord-mcp `--mcp-config` file (the DEVICE loopback-proxy shape).
/// The identity shim's `claude` wrapper reads it and appends
/// `--mcp-config $that` to the real argv — the universal delivery seam that gives
/// EVERY device-scope session (button-spawned, restore-re-spawned, hand-typed in
/// an arbitrary cwd, fresh install) coord-mcp with no `.mcp.json` in the cwd and
/// no user setup. Empty/unset ⇒ the shim appends nothing (fail-open — the session
/// simply has no coord-mcp, never a broken/FAILED server). Parallel to
/// [`crate::session::claude_hook::CLAUDE_SETTINGS_ENV`].
pub(crate) const MCP_CONFIG_ENV: &str = "QONTINUI_MCP_CONFIG";

/// Write a SHORT, single-line degraded breadcrumb into `workdir` (Phase 1a).
/// Emitted ONLY when coord-mcp is degraded — a healthy session writes nothing
/// (the file is removed by [`clear_degraded_breadcrumb`] on a successful probe),
/// so its mere presence is the signal. Best-effort: a write failure only logs.
pub(crate) fn write_degraded_breadcrumb(workdir: &str, reason: &str) {
    let line =
        format!("coord-mcp UNREACHABLE ({reason}) — gate registration degraded; use /gate\n");
    let path = Path::new(workdir).join(COORD_MCP_STATUS_FILE);
    if let Err(e) = std::fs::write(&path, line) {
        warn!("coord_mcp: failed to write degraded breadcrumb in {workdir}: {e}");
    }
}

/// Remove a stale degraded breadcrumb once coord-mcp is confirmed reachable, so
/// a session that recovered (e.g. a reconcile fixed the port) does not keep
/// showing a stale UNREACHABLE marker. Best-effort + idempotent (absent = ok).
fn clear_degraded_breadcrumb(workdir: &str) {
    let path = Path::new(workdir).join(COORD_MCP_STATUS_FILE);
    let _ = std::fs::remove_file(path);
}

/// One-shot, non-blocking coord-mcp reachability probe (Phase 1a). Fires a
/// `tools/list` JSON-RPC at the configured loopback proxy
/// (`http://127.0.0.1:<port>/coord-mcp`) carrying the session's nonce header,
/// with a short timeout, on a DETACHED thread so it never blocks or panics
/// session provisioning. On a non-2xx / transport failure it drops the
/// degraded breadcrumb; on success it clears any stale one and writes nothing.
pub(crate) fn probe_and_breadcrumb_proxy(workdir: &str, port: u16) {
    // Resolve the nonce we just wrote for this workdir so the probe authenticates
    // exactly as the session's MCP client will.
    let nonce = {
        let map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.iter()
            .find(|(_, b)| b.workdir.as_str() == workdir)
            .map(|(n, _)| n.clone())
    };
    let Some(nonce) = nonce else {
        return; // no nonce → nothing to probe against (already handled upstream)
    };
    let workdir = workdir.to_string();
    std::thread::spawn(move || {
        let url = format!("http://127.0.0.1:{port}/coord-mcp");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        });
        let client = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
        {
            Ok(c) => c,
            Err(_) => return, // can't build a client — skip silently (best-effort)
        };
        let reachable = client
            .post(&url)
            .header(COORD_MCP_PROXY_KEY_HEADER, &nonce)
            .json(&body)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if reachable {
            clear_degraded_breadcrumb(&workdir);
        } else {
            write_degraded_breadcrumb(
                &workdir,
                &format!("port :{port} probe failed (dead port | 401 stale nonce | coord down)"),
            );
        }
    });
}

fn write_mcp_json(primary_wt: &str, mcp_config: &serde_json::Value) {
    let mcp_path = Path::new(primary_wt).join(".mcp.json");
    match std::fs::write(
        &mcp_path,
        serde_json::to_string_pretty(mcp_config).unwrap_or_default(),
    ) {
        Ok(()) => {
            info!("coord_mcp: wrote .mcp.json for coord-mcp in {}", primary_wt);
        }
        Err(e) => {
            warn!(
                "coord_mcp: failed to write .mcp.json in {}: {e}",
                primary_wt
            );
        }
    }
}

/// Provision `.mcp.json` for a runner-spawned session that did NOT arrive with a
/// coord-minted agent JWT — i.e. gate-continuation terminals. Uses the runner's
/// own live **device** JWT (the same `SubType::Device` EdDSA token the coord
/// `/mcp` relay presents) so the continuation can use `coord_register_gate` over
/// MCP and `coord-acting-bearer.sh` (which mints an acting-user Service token
/// from it). This closes the reach gap where continuations — a primary place
/// follow-up gates get registered — otherwise fell back to the operator-bearer
/// stopgap (plan 2026-06-09-provision-coord-mcp-on-all-runner-spawned-sessions).
///
/// Two guards the agent-spawn path does not need (it always writes a coord agent
/// JWT into a fresh worktree):
/// 1. **sub_type guard.** The `access_token` slot can hold a *Cognito* token
///    rather than the coord device JWT on some pairing tiers
///    (`device_jwt_refresher`); a Cognito bearer would 401 against coord's EdDSA
///    verifier. Write only when the bearer decodes as `sub_type ∈ {device,
///    agent}`; otherwise log + skip (never write a non-verifying bearer).
/// 2. **non-clobber guard.** A continuation can degrade to a canonical repo
///    checkout (worktree mode off / acquire declined), which may already hold
///    the operator's own `.mcp.json`. Overwrite only when the file is absent or
///    is solely our `coord-mcp` config (see [`coord_mcp_safe_to_write`]).
// `pub(crate)` so the operator-opened-tab entry point
// (`commands::terminal::terminal_create`) reuses this exact helper — closing the
// last dark runner-spawned session kind (plan
// 2026-06-09-provision-coord-mcp-operator-tabs). Both modules compile into the
// runner bin crate, so `pub(crate)` is the minimal visibility that reaches it.
pub(crate) fn provision_coord_mcp_for_session(workdir: &str, bound_port: Option<u16>) {
    let jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            info!(
                "coord_mcp: no device JWT in access_token slot — skipping \
                 coord-mcp provisioning for {workdir}"
            );
            // 1a breadcrumb: a session with no device JWT cannot reach coord —
            // surface it so the agent self-routes to `/gate` instead of finding
            // a silently-absent MCP tool.
            write_degraded_breadcrumb(
                workdir,
                "no device JWT in runner access_token slot — coord-mcp not provisioned",
            );
            return;
        }
    };

    provision_coord_mcp_with_jwt(workdir, &jwt, bound_port);
}

/// Apply an already-resolved bearer to `workdir`'s `.mcp.json`, enforcing the two
/// guards: the bearer must decode `sub_type ∈ {device, agent}` (never write a
/// non-coord-verifying token), and the non-clobber / no-downgrade guard
/// ([`coord_mcp_safe_to_write`]) must allow it. Split out from
/// [`provision_coord_mcp_for_session`] so the write-decision orchestration is
/// unit-testable with a synthetic JWT — the live credential lives in the
/// encrypted `AuthManager` slot, which a unit test cannot seed deterministically
/// (and which would otherwise make the test pass/fail by whether the host happens
/// to be paired).
///
/// Device/agent split (live-token-proxy plan): `sub_type=device` emits the
/// loopback PROXY shape ([`write_coord_mcp_proxy_config`] on `bound_port`) so
/// the session reads a live device JWT per request; `sub_type=agent` keeps the
/// static-bearer shape UNCHANGED — agent JWTs are deliberately narrower than
/// the device JWT the proxy injects, so an agent session must never be routed
/// through the proxy (scope elevation).
fn provision_coord_mcp_with_jwt(workdir: &str, jwt: &str, bound_port: Option<u16>) {
    let sub_type = jwt_unverified_claim(jwt, "sub_type");
    match sub_type.as_deref() {
        Some("device") | Some("agent") => {}
        other => {
            info!(
                "coord_mcp: access_token bearer is not a coord device/agent \
                 JWT (sub_type={other:?}) — skipping coord-mcp provisioning for \
                 {workdir} (would 401 against coord's EdDSA verifier)"
            );
            return;
        }
    }

    if !coord_mcp_safe_to_write(workdir) {
        info!(
            "coord_mcp: {workdir}/.mcp.json already holds a non-coord-mcp \
             config — leaving it untouched (no coord-mcp provisioning)"
        );
        return;
    }

    if sub_type.as_deref() == Some("device") {
        // Phase 3a — fail-closed on an unknown bound port. The device path
        // writes a loopback proxy URL; if we can't resolve the ACTUALLY-BOUND
        // port we must NOT write a config pointing at the bootstrap default
        // (`:9876`), which is wrong on any secondary/temp runner and produces a
        // dead-but-valid-looking config (the F1 root cause). Refuse, warn, and
        // drop a 1a breadcrumb so the agent routes to `/gate`.
        let port = match bound_port {
            Some(p) => p,
            None => {
                warn!(
                    "coord_mcp: refusing to write a proxy .mcp.json for {workdir} — \
                     the bound API port is unresolvable (no managed AppState); a \
                     bootstrap-default port would be dead on a secondary runner. \
                     Run `coord doctor` / reprovision once the runtime is up."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "bound API port unresolvable — proxy config NOT written (would point at a dead port)",
                );
                return;
            }
        };
        write_coord_mcp_proxy_config(workdir, port);
        // 1a — one-shot, non-blocking reachability probe; writes a breadcrumb
        // only on failure, nothing on success (discoverability without clutter).
        probe_and_breadcrumb_proxy(workdir, port);
    } else {
        // AGENT path. In production this arm is effectively unreachable: the
        // only callers (`provision_coord_mcp_for_session` ← gate-continuation +
        // the `acquire_for_terminal` terminal chokepoint) read the bearer from
        // the runner's DEVICE access_token slot, which never holds an agent JWT
        // (agent JWTs are minted by coord and delivered in `LaunchPayload.jwt`,
        // provisioned at the spawn site, not here). It is kept defensively and
        // is exercised only by `provision_with_jwt_orchestration`.
        //
        // Even so we use the per-agent PROXY shape (delete-over-deprecate): bake
        // a `SharedToken` from the JWT claims, register it, and write the agent
        // proxy config. LIMITATION: this path has NO heartbeat driver, so the
        // slot is refreshed ONLY lazily on the request path (`maybe_refresh` in
        // the handler). For a steadily-used MCP client that is sufficient (the
        // 30-min margin ≫ inter-call gap); an idle agent that goes >TTL between
        // coord-mcp calls could still expire (coord's refresh endpoint rejects
        // an already-expired token — OQ4). The spawn-site path (with the 30s
        // heartbeat refresh) is the one that matters and never expires.
        // Fail closed on a missing/unparseable `sub` (the agent_id): registering a
        // `Uuid::nil()`-keyed slot would let two malformed-JWT provisions collide on
        // the same nil key (the second overwriting the first's slot). The trusted
        // spawn site (`agent_runtime::run_agent_subprocess`) keys on `payload.agent_id`
        // and never reaches here; this arm only ever sees a well-formed agent JWT in
        // tests, so a nil `sub` means a malformed token — refuse rather than register
        // an ambiguous slot.
        let agent_id = match jwt_unverified_claim(jwt, "sub").and_then(|s| Uuid::parse_str(&s).ok())
        {
            Some(id) => id,
            None => {
                warn!(
                    "coord_mcp: refusing to write an AGENT proxy .mcp.json for {workdir} — \
                     the agent JWT has no parseable `sub` (agent_id) claim."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "agent JWT missing a parseable `sub` (agent_id) — proxy config NOT written",
                );
                return;
            }
        };
        let exp = jwt_unverified_claim(jwt, "exp")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let port = match bound_port {
            Some(p) => p,
            None => {
                warn!(
                    "coord_mcp: refusing to write an AGENT proxy .mcp.json for {workdir} — \
                     the bound API port is unresolvable (no managed AppState)."
                );
                write_degraded_breadcrumb(
                    workdir,
                    "bound API port unresolvable — agent proxy config NOT written (would point at a dead port)",
                );
                return;
            }
        };
        let slot = std::sync::Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
            token: jwt.to_string(),
            jti: Uuid::nil(),
            exp,
        }));
        register_agent_token(agent_id, slot);
        write_coord_mcp_agent_proxy_config(workdir, port, agent_id);
        probe_and_breadcrumb_proxy(workdir, port);
    }
}

/// Decode an unverified string claim from a JWT payload. We do NOT verify the
/// signature — coord re-validates the EdDSA signature on use; here we only need
/// `sub_type` to avoid writing a non-coord-verifying bearer (e.g. a Cognito
/// token) into `.mcp.json`. Mirror of `cognito::jwt_claim`, inlined because that
/// fn lives in the lib crate (`pub(crate)`) and `agent_runtime` compiles into
/// the bin crate, which does not re-declare `mod cognito`.
fn jwt_unverified_claim(token: &str, claim: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut parts = token.splitn(3, '.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _sig = parts.next()?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get(claim).and_then(|v| v.as_str()).map(String::from)
}

/// True iff writing our coord-mcp `.mcp.json` into `workdir` would not clobber a
/// user's own config: the file is absent/unreadable, OR it parses as a config
/// whose `mcpServers` is solely our `coord-mcp` entry (a prior provisioning we
/// own and may refresh). A foreign or unparseable file returns false (leave it).
fn coord_mcp_safe_to_write(workdir: &str) -> bool {
    let path = Path::new(workdir).join(".mcp.json");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return true, // absent (or unreadable) → safe to create
    };
    let parsed: serde_json::Value = match serde_json::from_str(&existing) {
        Ok(v) => v,
        Err(_) => return false, // unparseable foreign file → do not clobber
    };
    match parsed.get("mcpServers").and_then(|m| m.as_object()) {
        Some(servers) => {
            if servers.len() == 1 && servers.contains_key("coord-mcp") {
                // Our own coord-mcp config — refreshable, EXCEPT never downgrade an
                // existing agent JWT (richer scopes) to a device JWT. If the current
                // bearer decodes sub_type=agent, leave it. The device-path PROXY
                // shape (loopback URL + nonce header) has NO Authorization header,
                // so it deliberately falls through this check as ours-refreshable.
                let existing_is_agent = parsed
                    .pointer("/mcpServers/coord-mcp/headers/Authorization")
                    .and_then(|v| v.as_str())
                    .and_then(|h| h.strip_prefix("Bearer "))
                    .and_then(|tok| jwt_unverified_claim(tok, "sub_type"))
                    .map(|st| st == "agent")
                    .unwrap_or(false);
                !existing_is_agent
            } else {
                false
            }
        }
        None => false,
    }
}

/// Read the loopback proxy port out of an existing coord-mcp `.mcp.json`, if the
/// file holds the PROXY shape (`url == http://127.0.0.1:<port>/coord-mcp`).
/// Returns `None` for an absent/unparseable file or a non-proxy (static-bearer)
/// shape — the latter is the agent path, which the reconcile must never touch.
fn read_proxy_port(workdir: &str) -> Option<u16> {
    let path = Path::new(workdir).join(".mcp.json");
    let s = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let url = v
        .pointer("/mcpServers/coord-mcp/url")
        .and_then(|u| u.as_str())?;
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = rest.strip_suffix("/coord-mcp")?;
    port_str.parse::<u16>().ok()
}

/// True iff `<workdir>/.mcp.json` already declares a `coord-mcp` server (ANY
/// shape — proxy, static-bearer, or a hand-authored operator entry). The identity
/// seam uses this to SKIP injecting its app-data `--mcp-config` when the cwd
/// already provides coord-mcp, so a session never ends up with two `coord-mcp`
/// entries competing for the per-workdir nonce (the loser 401s and the client
/// marks it FAILED). Covers three real cases at once:
///   * a gate-continuation terminal, whose device `.mcp.json` is written by
///     [`provision_coord_mcp_for_session`] BEFORE the terminal spawn/seam runs;
///   * the operator's own repo-root `.mcp.json` (boot self-heal keeps it on the
///     bound proxy port);
///   * any session re-spawned into a cwd a prior provision already wrote.
/// A cwd with NO `.mcp.json`, or one whose `.mcp.json` has only the user's OWN
/// (non-coord) servers, returns `false` → the seam injects `--mcp-config`, which
/// merges additively without touching the user's file.
pub(crate) fn workdir_declares_coord_mcp(workdir: &str) -> bool {
    let path = Path::new(workdir).join(".mcp.json");
    let Ok(s) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return false;
    };
    v.pointer("/mcpServers/coord-mcp").is_some()
}

/// Stable per-workdir filename for the runner-owned coord-mcp `--mcp-config` file.
/// A non-cryptographic hash of the absolute workdir keeps the name short (Windows
/// path limits) and collision-free in practice, and STABLE across re-spawns into
/// the same cwd so a restart reuses one path (rewritten with the fresh nonce).
fn mcp_config_file_name(workdir: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    workdir.hash(&mut h);
    format!("coord-mcp-{:016x}.json", h.finish())
}

/// Materialize a DEVICE-scope coord-mcp `--mcp-config` file into the runner's OWN
/// app-data dir (`~/.qontinui/runner/session-restore/coord-mcp/`, NEVER the cwd)
/// and return its absolute path, so the identity shim can append
/// `--mcp-config <path>` to a claude launch that would not otherwise get coord-mcp
/// (a plain/hand-typed terminal in an arbitrary cwd, a restore-re-spawned session,
/// a fresh install). Mirrors the `--settings` hook delivery
/// ([`crate::session::claude_hook::materialize`]): a runner-owned file + an env
/// var, never touching `~/.claude` nor the user's cwd.
///
/// DEVICE principal ONLY. The identity seam ([`TerminalSession::apply_identity_seam`])
/// runs exclusively for interactive + gate-continuation terminals (device scope);
/// headless agent subprocesses launch through a separate direct-`tokio` spawn
/// (`agent_runtime::run_agent_subprocess` → `spawn_claude_child`) that never
/// reaches the seam and provisions its own AGENT proxy config in-worktree. So this
/// can never elevate an agent session onto the device JWT (the scope-elevation
/// trap [`write_coord_mcp_agent_proxy_config`] guards against).
///
/// Fail-closed (Phase 3a) on an unresolvable bound API port: returns `None` rather
/// than writing a config pointing at the dead bootstrap-default `:9876` (wrong on
/// any secondary/temp runner — the F1 root cause). The caller (the seam) then sets
/// no env var, so the shim appends nothing — fail-open to NO coord-mcp, never a
/// connection-refused server the client marks FAILED, and NO breadcrumb written
/// into the user's cwd (the pollution non-goal). The nonce is DEVICE-principal and
/// persisted ([`register_proxy_nonce`]), so an already-written file keeps
/// validating across an orphan-outliving-a-restart edge.
pub(crate) fn provision_coord_mcp_config_file(workdir: &str) -> Option<std::path::PathBuf> {
    // Phase 3a fail-closed: never point --mcp-config at a bootstrap-default port.
    let bound_port = resolve_bound_api_port()?;
    let nonce = register_proxy_nonce(workdir); // DEVICE principal, persisted
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": format!("http://127.0.0.1:{bound_port}/coord-mcp"),
                "headers": {
                    "X-Coord-Mcp-Proxy-Key": nonce,
                }
            }
        }
    });
    let dir = crate::session::claude_hook::session_restore_dir().join("coord-mcp");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        warn!(
            "coord_mcp: failed to create app-data mcp-config dir {}: {e} — \
             --mcp-config delivery off for {workdir} (session simply has no coord-mcp)",
            dir.display()
        );
        return None;
    }
    let file = dir.join(mcp_config_file_name(workdir));
    match std::fs::write(
        &file,
        serde_json::to_string_pretty(&mcp_config).unwrap_or_default(),
    ) {
        Ok(()) => {
            info!(
                "coord_mcp: wrote --mcp-config file {} for workdir {workdir} (bound :{bound_port})",
                file.display()
            );
            Some(file)
        }
        Err(e) => {
            warn!(
                "coord_mcp: failed to write --mcp-config file {}: {e} — \
                 --mcp-config delivery off for {workdir}",
                file.display()
            );
            None
        }
    }
}

/// Read the per-session proxy NONCE out of an existing coord-mcp `.mcp.json`, if
/// the file holds the PROXY shape (an `X-Coord-Mcp-Proxy-Key` header). Returns
/// `None` for an absent/unparseable file or a non-proxy shape. Used by the
/// root-config self-heal: a nonce no longer in the live registry (evicted on a
/// re-provision, or simply never restored) means the config would 401 the
/// proxy, so it must be rewritten even when the port still matches.
fn read_proxy_nonce(config_path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(config_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.pointer("/mcpServers/coord-mcp/headers/X-Coord-Mcp-Proxy-Key")
        .and_then(|n| n.as_str())
        .map(String::from)
}

/// Boot-time reconcile decision for one session's `.mcp.json` (Phase 3c). Pure
/// over its inputs (no I/O) so the rewrite predicate is unit-testable:
///
/// - `Rewrite` — the config holds the proxy shape on a port ≠ the instance's
///   current bound port → rewrite it to the correct port (+ a fresh persisted
///   nonce) so the next MCP read targets a live proxy.
/// - `Leave` — no `.mcp.json` proxy port readable, OR the port already matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReconcileAction {
    Rewrite,
    Leave,
}

/// Pure reconcile predicate: given the session's currently-written proxy port
/// (if any) and the instance's current bound port, decide whether to rewrite.
pub(crate) fn reconcile_action(
    current_proxy_port: Option<u16>,
    bound_port: u16,
) -> ReconcileAction {
    match current_proxy_port {
        Some(p) if p != bound_port => ReconcileAction::Rewrite,
        _ => ReconcileAction::Leave,
    }
}

/// Boot-time self-heal decision for the ROOT `.mcp.json` (plan
/// 2026-07-07-coord-mcp-nonce-survives-runner-restart, Change 1). Finer-grained
/// than [`ReconcileAction`] because the root path can heal WITHOUT rewriting the
/// file — which is the whole point: a live MCP client caches the nonce from
/// `.mcp.json` at connect and NEVER re-reads it, so any file rewrite (even a
/// byte-different one on the same port) strands that client on a nonce the new
/// registry evicted. Adopting the on-disk nonce instead keeps the file
/// byte-identical, so the client's cached nonce keeps validating across a
/// restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootReconcileAction {
    /// Healthy (port matches AND the on-disk nonce is currently registered), or
    /// not our config to touch (absent / static-bearer shape) — do nothing.
    Leave,
    /// Adopt the exact on-disk nonce into the live registry as this workdir's
    /// Device binding — NO file rewrite. Produced only when the proxy port still
    /// matches, a non-empty nonce IS readable from disk, but that nonce is not in
    /// the live registry (evicted on a re-provision, or never restored on boot).
    AdoptNonce,
    /// Mint a fresh nonce and rewrite the file to the current bound port.
    /// Produced when the port moved (a live client must reconnect regardless, so
    /// preserving the old nonce buys nothing) OR no nonce is readable from disk.
    Rewrite,
}

/// Pure resolver for [`RootReconcileAction`] — decoupled from file I/O so the
/// adopt-vs-rewrite decision is unit-testable against explicit inputs. Inputs:
/// the proxy port currently written to the root file (`None` = absent /
/// unparseable / static-bearer shape), the nonce string readable from that file
/// (`None` = no proxy-key header), whether that nonce is currently in the live
/// registry, and the instance's bound port.
pub(crate) fn root_reconcile_action(
    current_proxy_port: Option<u16>,
    on_disk_nonce: Option<&str>,
    nonce_is_registered: bool,
    bound_port: u16,
) -> RootReconcileAction {
    let Some(port) = current_proxy_port else {
        // Not our proxy shape (absent / unparseable / static-bearer agent
        // config) — never touched here.
        return RootReconcileAction::Leave;
    };
    if port != bound_port {
        // Port moved: the client's cached URL is stale too, so it must reconnect
        // to reach the live port regardless — mint fresh + rewrite.
        return RootReconcileAction::Rewrite;
    }
    // Port matches — the only remaining staleness is the nonce.
    match on_disk_nonce {
        // A non-empty nonce is readable but not registered → adopt it so the
        // live client's cached nonce validates again without a file change.
        Some(nonce) if !nonce.is_empty() && !nonce_is_registered => RootReconcileAction::AdoptNonce,
        // A registered nonce → healthy, nothing to do.
        Some(_) if nonce_is_registered => RootReconcileAction::Leave,
        // No nonce readable (or an empty one) → nothing to adopt; mint fresh.
        _ => RootReconcileAction::Rewrite,
    }
}

/// Resolve the `qontinui-root` directory the checked-in repo-root `.mcp.json`
/// lives in. Mirror of `agent_runtime::qontinui_root_dir`, inlined so this leaf
/// module does NOT depend on `agent_runtime` (which depends back on us — the
/// cycle this module was extracted to break). `QONTINUI_ROOT` env first, then
/// the Windows `D:/qontinui-root` well-known path, then `~/qontinui-root`.
fn qontinui_root_dir() -> Option<std::path::PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = std::path::PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = std::path::PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("qontinui-root"))
        .filter(|p| p.is_dir())
}

/// Re-register an EXISTING on-disk proxy nonce string into the live registry as
/// a Device binding for `workdir`, WITHOUT minting a new nonce or rewriting the
/// file (plan 2026-07-07-coord-mcp-nonce-survives-runner-restart, Change 1).
/// This is the restart-resilient self-heal: when the root `.mcp.json` proxy port
/// still matches but its nonce was evicted / never restored, adopting the exact
/// on-disk string keeps a live MCP client's CACHED nonce validating across the
/// restart (the client never re-reads the file, so a fresh-minted-and-rewritten
/// nonce would strand it on a 401). Evicts any prior nonce for the same workdir
/// (there should be none) and mirrors the updated set to the encrypted store so
/// the adoption itself survives the NEXT restart. DEVICE binding only — this
/// path is never reached for an agent config (a static-bearer shape has no proxy
/// URL, so [`read_proxy_port`] returns `None` and the resolver leaves it).
fn adopt_on_disk_nonce(workdir: &str, nonce: &str) {
    let snapshot = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.retain(|_, b| b.workdir != workdir);
        map.insert(
            nonce.to_string(),
            NonceBinding {
                workdir: workdir.to_string(),
                principal: ProxyPrincipal::Device,
            },
        );
        map.clone()
    };
    persist_proxy_nonces(&snapshot);
}

/// Read the root `.mcp.json` ONCE and resolve both the self-heal action and the
/// nonce string readable from it (the [`RootReconcileAction::AdoptNonce`] arm
/// needs that string to re-register). Keeps the file read out of the pure
/// [`root_reconcile_action`] resolver while giving both callers
/// ([`root_config_is_stale`] and [`reconcile_root_config_at`]) a single,
/// consistent read.
fn resolve_root_reconcile(
    root_dir: &Path,
    bound_port: u16,
) -> (RootReconcileAction, Option<String>) {
    let path = root_dir.join(".mcp.json");
    let current_port = read_proxy_port(&root_dir.to_string_lossy());
    let on_disk_nonce = read_proxy_nonce(&path);
    let registered = on_disk_nonce
        .as_deref()
        .map(proxy_nonce_is_valid)
        .unwrap_or(false);
    let action = root_reconcile_action(
        current_port,
        on_disk_nonce.as_deref(),
        registered,
        bound_port,
    );
    (action, on_disk_nonce)
}

/// True iff the root `.mcp.json` at `root_dir` needs SOME self-heal action
/// (adopt-nonce or rewrite) for the current `bound_port`. Returns `false` (leave
/// it) for an absent file, a non-proxy (static-bearer) shape, or a proxy config
/// whose port matches AND whose nonce is currently registered. Delegates to the
/// single-read [`resolve_root_reconcile`] so the "is anything to do" predicate
/// and the "what to do" dispatch never diverge. Test-only convenience — the prod
/// dispatch reads the action directly from [`resolve_root_reconcile`].
#[cfg(test)]
fn root_config_is_stale(root_dir: &Path, bound_port: u16) -> bool {
    resolve_root_reconcile(root_dir, bound_port).0 != RootReconcileAction::Leave
}

/// Boot-time self-heal of the CHECKED-IN repo-root `.mcp.json` (Phase 5b),
/// resolving the root dir from the environment. Delegates to
/// [`reconcile_root_config_at`] — split so the heal is unit-testable against
/// an explicit temp dir WITHOUT mutating the process-global `QONTINUI_ROOT`
/// (the module deliberately avoids global-env mutation; see the test helpers).
///
/// `pub(crate)` so the boot task can call root self-heal UNCONDITIONALLY —
/// independent of whether any live session workdir is present (plan
/// 2026-07-07 Change 1 secondary gap: a boot with zero open sessions must still
/// repair a stale-port root config, which the old session-gated wiring skipped).
/// Returns the [`RootReconcileAction`] actually taken (`Leave` when there is no
/// resolvable root dir) so the boot task can log the restore-vs-heal outcome.
pub(crate) fn reconcile_root_config(bound_port: u16) -> RootReconcileAction {
    match qontinui_root_dir() {
        Some(root_dir) => reconcile_root_config_at(&root_dir, bound_port),
        None => RootReconcileAction::Leave,
    }
}

/// Self-heal the repo-root `.mcp.json` under `root_dir` (Phase 5b + plan
/// 2026-07-07 Change 1). The root config is the loopback PROXY shape (port +
/// per-session nonce); a spawned agent that inherits it (cwd up the tree, no
/// per-worktree config) — and a long-running operator session whose MCP client
/// cached the nonce — break if the nonce was evicted or the port moved across a
/// restart/instance change. Dispatches on [`resolve_root_reconcile`]:
///
/// - `AdoptNonce` — port unchanged, a nonce IS on disk but unregistered:
///   re-register that EXACT string ([`adopt_on_disk_nonce`]), leaving the file
///   byte-identical so a live client's cached nonce keeps validating. Returns
///   `false` (no file rewrite).
/// - `Rewrite` — port moved (client must reconnect regardless) or no nonce
///   readable: mint fresh + rewrite via [`write_coord_mcp_proxy_config`].
///   Returns `true`.
/// - `Leave` — healthy or not ours. Returns `false`.
///
/// Both mutating arms are guarded by [`coord_mcp_safe_to_write`] so a
/// hand-rolled static-bearer root file is never clobbered. Returns the
/// [`RootReconcileAction`] actually applied — a guard refusal downgrades to
/// `Leave` (nothing happened), so the return is an honest record of the effect.
fn reconcile_root_config_at(root_dir: &Path, bound_port: u16) -> RootReconcileAction {
    let (action, on_disk_nonce) = resolve_root_reconcile(root_dir, bound_port);
    let root = root_dir.to_string_lossy().to_string();
    match action {
        RootReconcileAction::Leave => RootReconcileAction::Leave,
        RootReconcileAction::AdoptNonce => {
            if !coord_mcp_safe_to_write(&root) {
                return RootReconcileAction::Leave;
            }
            // SAFETY: the resolver only yields AdoptNonce when a non-empty nonce
            // was read from disk, so this Option is always Some here.
            let nonce = on_disk_nonce
                .expect("AdoptNonce implies a readable on-disk nonce (resolver invariant)");
            adopt_on_disk_nonce(&root, &nonce);
            info!(
                "coord_mcp: boot self-heal ADOPTED on-disk root nonce for {root} \
                 (port :{bound_port} unchanged; .mcp.json byte-identical) — live \
                 MCP client cache preserved"
            );
            RootReconcileAction::AdoptNonce
        }
        RootReconcileAction::Rewrite => {
            if !coord_mcp_safe_to_write(&root) {
                return RootReconcileAction::Leave;
            }
            write_coord_mcp_proxy_config(&root, bound_port);
            info!(
                "coord_mcp: boot self-heal rewrote root {root}/.mcp.json to bound \
                 port :{bound_port} (fresh nonce — port moved or no on-disk nonce)"
            );
            RootReconcileAction::Rewrite
        }
    }
}

/// Boot-time session-config reconcile (Phase 3c). For each live session workdir,
/// if its `.mcp.json` coord-mcp proxy port ≠ the instance's CURRENT bound port,
/// rewrite it via [`write_coord_mcp_proxy_config`] (correct port + a freshly
/// persisted nonce), guarded by [`coord_mcp_safe_to_write`] so it never clobbers
/// an agent-spawn's static-bearer config. Combined with Phase 3b (persisted
/// nonces), the common same-port restart needs no rewrite at all — this covers
/// only the instance/port-change case. Returns the number of SESSION configs
/// rewritten.
///
/// Root-config self-heal is NOT done here — the boot task calls
/// [`reconcile_root_config`] UNCONDITIONALLY (plan 2026-07-07 Change 1 secondary
/// gap: a boot with zero open sessions must still repair the root config, which
/// the old session-gated tail-call skipped). Keeping the two concerns separate
/// lets the caller heal the root even when this fn is not called at all.
///
/// Wired into the same startup path as the other auto-start tasks (see
/// `mcp_api::start_server`), AFTER [`restore_proxy_nonces_from_store`] so a
/// rewrite reuses the restored map where possible.
pub(crate) fn reconcile_session_configs<I>(workdirs: I, bound_port: u16) -> usize
where
    I: IntoIterator<Item = String>,
{
    let mut rewritten = 0usize;
    for workdir in workdirs {
        if reconcile_action(read_proxy_port(&workdir), bound_port) != ReconcileAction::Rewrite {
            continue;
        }
        if !coord_mcp_safe_to_write(&workdir) {
            // An agent-spawn static-bearer config (or a user's own file) — never
            // clobber. (A proxy-shaped config is ours-refreshable, so this only
            // skips configs we must not touch.)
            continue;
        }
        write_coord_mcp_proxy_config(&workdir, bound_port);
        rewritten += 1;
        info!("coord_mcp: reconciled {workdir}/.mcp.json to bound port :{bound_port}");
    }
    if rewritten > 0 {
        info!(
            "coord_mcp: boot reconcile rewrote {rewritten} session config(s) to the current bound port"
        );
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `workdir_declares_coord_mcp` — the identity-seam skip gate — is true iff
    /// `<workdir>/.mcp.json` declares a `coord-mcp` server (any shape), false for
    /// absent / unparseable / non-coord `.mcp.json`.
    #[test]
    fn workdir_declares_coord_mcp_detects_any_coord_entry() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-declares-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        // Absent → false (seam injects).
        assert!(!workdir_declares_coord_mcp(&wd));

        // A user's OWN non-coord servers → false (seam injects --mcp-config,
        // merging additively without touching their file).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"my-server":{"type":"stdio","command":"x"}}}"#,
        )
        .unwrap();
        assert!(!workdir_declares_coord_mcp(&wd));

        // A coord-mcp proxy entry → true (seam skips — continuation terminal /
        // operator root / previously-provisioned cwd already provides it).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp"}}}"#,
        )
        .unwrap();
        assert!(workdir_declares_coord_mcp(&wd));

        // Unparseable → false (never a false-skip on a garbage file).
        std::fs::write(&mcp, "not json {").unwrap();
        assert!(!workdir_declares_coord_mcp(&wd));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Phase 3a fail-closed: with no Tauri runtime / managed AppState in a unit
    /// test, `resolve_bound_api_port` returns `None`, so `provision_coord_mcp_config_file`
    /// writes NOTHING and returns `None` — the seam then injects no
    /// QONTINUI_MCP_CONFIG (fail-open to no coord-mcp, never a dead-port config,
    /// and — critically — no breadcrumb written into the user's cwd).
    #[test]
    fn provision_coord_mcp_config_file_fail_closed_without_bound_port() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-provfile-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();

        assert!(
            provision_coord_mcp_config_file(&wd).is_none(),
            "no bound port ⇒ no --mcp-config file (fail-closed)"
        );
        // And no cwd pollution — the degraded breadcrumb belongs to the workdir
        // `.mcp.json` path, never the seam's arbitrary-cwd delivery.
        assert!(
            !dir.join(COORD_MCP_STATUS_FILE).exists(),
            "seam fail-closed must not pollute the cwd with a breadcrumb"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The per-workdir `--mcp-config` filename is stable for a given workdir and
    /// distinct across workdirs (so two cwds never collide on one app-data file).
    #[test]
    fn mcp_config_file_name_is_stable_and_workdir_distinct() {
        let a1 = mcp_config_file_name("D:/repo/one");
        let a2 = mcp_config_file_name("D:/repo/one");
        let b = mcp_config_file_name("D:/repo/two");
        assert_eq!(a1, a2, "stable across calls for one workdir");
        assert_ne!(a1, b, "distinct across workdirs");
        assert!(a1.starts_with("coord-mcp-") && a1.ends_with(".json"));
    }

    /// A temp-dir-backed [`SecureStorage`] for the persistence tests — injected
    /// directly into the `_with_store` seam so a test NEVER mutates the
    /// process-global `QONTINUI_SECURE_STORAGE_DIR`. Mutating that env var raced
    /// sibling tests that read the DEFAULT store (notably
    /// `auth::device_jwt_tests::needs_refresh_when_no_token`), which is why this
    /// module no longer touches global env at all.
    fn temp_store(tag: &str) -> (std::path::PathBuf, crate::secure_storage::SecureStorage) {
        let dir =
            std::env::temp_dir().join(format!("coord-mcp-{tag}-store-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = crate::secure_storage::SecureStorage::with_path(dir.join("nonces.enc"))
            .expect("temp secure storage");
        (dir, store)
    }

    /// The DEVICE-path proxy shape: loopback URL built from the passed bound
    /// port, a registered per-session nonce header, and — critically — NO
    /// baked `Authorization` bearer (the proxy injects a live one per request).
    #[test]
    fn write_coord_mcp_proxy_config_emits_loopback_nonce_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-proxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let primary_wt = tmp.to_string_lossy().to_string();

        write_coord_mcp_proxy_config(&primary_wt, 23456);

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];

        assert_eq!(server["type"], "http");
        assert_eq!(
            server["url"], "http://127.0.0.1:23456/coord-mcp",
            "URL must target the loopback proxy on the PASSED bound port"
        );

        // The nonce header is present, non-empty, and live in the registry.
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("proxy config must carry the per-session nonce header");
        assert!(!nonce.is_empty());
        assert!(
            proxy_nonce_is_valid(nonce),
            "the written nonce must be registered for the proxy gate"
        );

        // NO baked bearer — the whole point is the proxy injects a live one.
        assert!(
            server["headers"].get("Authorization").is_none(),
            "proxy shape must NOT bake a static Authorization bearer: {written}"
        );

        // Re-provisioning the same workdir mints a FRESH nonce and moves the
        // prior one onto the grace TTL (plan 2026-07-07 Change 3): the old nonce
        // is dropped from the LIVE map but stays valid briefly so an in-flight
        // client that cached it rides through until it reconnects — rather than
        // hard-401ing the instant `.mcp.json` is rewritten. Both nonces resolve
        // to the same Device principal, so there is no scope-elevation surface.
        write_coord_mcp_proxy_config(&primary_wt, 23456);
        let reprovisioned = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v2: serde_json::Value = serde_json::from_str(&reprovisioned).unwrap();
        let new_nonce = v2["mcpServers"]["coord-mcp"]["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("re-provision must carry a nonce header");
        assert_ne!(new_nonce, nonce, "a re-provision must mint a fresh nonce");
        assert!(
            proxy_nonce_is_valid(new_nonce),
            "the freshly-minted nonce is live in the registry"
        );
        assert!(
            proxy_nonce_is_valid(nonce),
            "the prior device nonce rides the grace TTL (Change 3) — not hard-evicted"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The loopback proxy's pre-forward gate over a DEVICE-bound nonce:
    /// registered nonce + device bearer → forward; everything else → 401 before
    /// any network I/O. This is the scope-elevation backstop — a device nonce
    /// must never forward a non-device bearer.
    #[test]
    fn proxy_request_gate_forwards_only_nonce_plus_device_bearer() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        let dir = std::env::temp_dir().join(format!("coord-mcp-gate-{}", uuid::Uuid::new_v4()));
        let nonce = register_proxy_nonce(&dir.to_string_lossy());
        let device = mk("device");
        let dev_p = ProxyPrincipal::Device;

        // The nonce resolves to a Device principal.
        assert_eq!(
            proxy_principal_for_nonce(&nonce),
            Some(ProxyPrincipal::Device)
        );

        // Registered nonce + device bearer → forward.
        assert!(proxy_request_gate(Some(&nonce), Some(&device), &dev_p).is_ok());

        // Absent / mismatched nonce → 401, regardless of bearer.
        assert_eq!(
            proxy_request_gate(None, Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some("not-a-registered-nonce"), Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some(""), Some(&device), &dev_p)
                .unwrap_err()
                .0,
            401
        );

        // Valid nonce but no/empty bearer → 401.
        assert_eq!(
            proxy_request_gate(Some(&nonce), None, &dev_p)
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(" "), &dev_p)
                .unwrap_err()
                .0,
            401
        );

        // Valid device nonce but an AGENT bearer → 401 (scope-elevation trap:
        // a device nonce must only ever attach the runner's DEVICE identity).
        let agent_err = proxy_request_gate(Some(&nonce), Some(&mk("agent")), &dev_p).unwrap_err();
        assert_eq!(agent_err.0, 401);
        // A non-coord (e.g. Cognito) bearer → 401 too.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(&mk("access")), &dev_p)
                .unwrap_err()
                .0,
            401
        );
        // A non-JWT string → 401, never a panic.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some("not-a-jwt"), &dev_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// The AGENT-bound nonce arm of the gate + the cross-binding rejections in
    /// BOTH directions (the structural scope-elevation backstop):
    ///  - an agent nonce resolves to `ProxyPrincipal::Agent` and forwards an
    ///    agent bearer, but REJECTS a device bearer;
    ///  - a device nonce REJECTS an agent bearer (covered above too, repeated
    ///    here for symmetry).
    #[test]
    fn proxy_request_gate_binds_agent_nonce_to_agent_bearer() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        let agent_id = uuid::Uuid::new_v4();
        let dir = std::env::temp_dir().join(format!("coord-mcp-agate-{}", uuid::Uuid::new_v4()));
        let agent_nonce = register_agent_proxy_nonce(&dir.to_string_lossy(), agent_id);

        // The nonce resolves to an Agent principal carrying the agent_id.
        assert_eq!(
            proxy_principal_for_nonce(&agent_nonce),
            Some(ProxyPrincipal::Agent { agent_id })
        );

        let agent_p = ProxyPrincipal::Agent { agent_id };
        let device_p = ProxyPrincipal::Device;

        // Agent nonce + agent bearer → forward.
        assert!(proxy_request_gate(Some(&agent_nonce), Some(&mk("agent")), &agent_p).is_ok());

        // Agent nonce + DEVICE bearer → 401 (an agent nonce can never inject the
        // device token — the reverse scope-elevation direction).
        assert_eq!(
            proxy_request_gate(Some(&agent_nonce), Some(&mk("device")), &agent_p)
                .unwrap_err()
                .0,
            401
        );

        // And a device principal must reject an agent bearer.
        let dev_dir =
            std::env::temp_dir().join(format!("coord-mcp-dgate-{}", uuid::Uuid::new_v4()));
        let dev_nonce = register_proxy_nonce(&dev_dir.to_string_lossy());
        assert_eq!(
            proxy_request_gate(Some(&dev_nonce), Some(&mk("agent")), &device_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// Phase 3 invariant pin (terminal-autonomy-survives-logout): a MISSING
    /// device JWT degrades the coord-mcp PROXY path to an actionable retry —
    /// and ONLY that path. Local terminal AI session work (model reasoning, the
    /// PTY, local tools) is structurally independent of this gate, so a coord
    /// credential gap can never block local work.
    ///
    /// This asserts the gating is scoped to coord-mcp proxy requests:
    ///  - [`proxy_request_gate`] is the ONLY thing a missing device JWT affects;
    ///    it governs proxy forwarding exclusively (it takes a proxy nonce +
    ///    bearer + the bound principal and returns only forward-or-401). It is
    ///    not on, and has no handle into, any local-execution path.
    ///  - the missing-device-JWT case degrades to an ACTIONABLE, retry-shaped
    ///    error ([`device_jwt_refreshing_error`]) — not a panic, not a hang
    ///    (the wait is bounded, see `await_remint_*` tests), not a bare 401.
    ///  - the gate's hard nonce/scope 401s stay FAIL-CLOSED and unchanged.
    ///
    /// Why local work is independent (documented here as the layer can't call a
    /// PTY): the terminal AI session's MODEL auth is the operator's own Claude
    /// subscription via `CLAUDE_CONFIG_DIR`, and the PTY + local tools never
    /// issue a coord-mcp proxy request — only an explicit coord MCP tool call
    /// routes through this gate. So a missing device JWT can at most make a
    /// coord tool call retry; it cannot stall the session's local progress.
    #[test]
    fn missing_device_jwt_degrades_proxy_only_not_local_work() {
        // (a) The degrade is an ACTIONABLE retry, distinct from the hard 401.
        let (status, msg) = device_jwt_refreshing_error();
        assert_eq!(status, 503, "transient credential gap → retryable, not 401");
        assert!(
            msg.to_lowercase().contains("retry"),
            "degrade message must tell the caller to retry: {msg:?}"
        );
        assert!(
            !msg.contains("no live JWT available"),
            "must NOT be the bare missing-JWT 401 message"
        );

        // (b) The gate is scoped to proxy requests only: its hard nonce/scope
        // 401s remain fail-closed and unchanged. A valid device nonce with NO
        // bearer is still a hard 401 backstop (the proxy handler only reaches
        // the gate AFTER the bounded re-mint produced a usable bearer).
        let dir = std::env::temp_dir().join(format!("coord-mcp-p3-{}", uuid::Uuid::new_v4()));
        let nonce = register_proxy_nonce(&dir.to_string_lossy());
        let dev_p = ProxyPrincipal::Device;
        assert_eq!(
            proxy_request_gate(Some(&nonce), None, &dev_p)
                .unwrap_err()
                .0,
            401,
            "missing bearer at the gate is still a hard 401 backstop"
        );
        // Bad nonce stays a hard 401 regardless of the degrade path.
        assert_eq!(
            proxy_request_gate(Some("nope"), None, &dev_p)
                .unwrap_err()
                .0,
            401
        );
    }

    /// The bounded re-mint wait TERMINATES on the deadline (never hangs) when no
    /// usable JWT ever appears — pins "NEVER block a request indefinitely".
    #[tokio::test]
    async fn await_remint_is_bounded_when_no_jwt_appears() {
        let calls = std::cell::Cell::new(0u32);
        let started = std::time::Instant::now();
        let out = await_remint_with(
            || {
                calls.set(calls.get() + 1);
                async { None }
            },
            std::time::Duration::from_millis(120),
            std::time::Duration::from_millis(20),
        )
        .await;
        assert_eq!(out, None, "no JWT → None after the bound");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "must return promptly at the bound, not hang"
        );
        assert!(calls.get() >= 2, "should have polled more than once");
    }

    /// The bounded re-mint wait RETURNS as soon as a freshly-minted JWT appears
    /// mid-poll — pins the common case: the refresher re-mints within the bound
    /// and the in-flight tool call proceeds normally.
    #[tokio::test]
    async fn await_remint_returns_jwt_once_it_appears() {
        let calls = std::cell::Cell::new(0u32);
        let out = await_remint_with(
            || {
                calls.set(calls.get() + 1);
                let n = calls.get();
                async move {
                    if n >= 3 {
                        Some("h.payload.s".to_string())
                    } else {
                        None
                    }
                }
            },
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(10),
        )
        .await;
        assert_eq!(out.as_deref(), Some("h.payload.s"));
        assert_eq!(calls.get(), 3, "returns on the first usable read, no more");
    }

    /// `AGENT_TOKENS` register / lookup / remove round-trip.
    #[test]
    fn agent_token_registry_round_trip() {
        let agent_id = uuid::Uuid::new_v4();
        assert!(lookup_agent_token(agent_id).is_none());
        let slot = std::sync::Arc::new(tokio::sync::RwLock::new(crate::agent_token::TokenSlot {
            token: "tok".into(),
            jti: uuid::Uuid::nil(),
            exp: 0,
        }));
        register_agent_token(agent_id, slot);
        assert!(lookup_agent_token(agent_id).is_some());
        remove_agent_token(agent_id);
        assert!(
            lookup_agent_token(agent_id).is_none(),
            "lookup after remove must be None (fail-closed)"
        );
    }

    /// The agent-proxy `.mcp.json` shape matches the device proxy shape:
    /// loopback URL + nonce header, no baked Authorization bearer. Only the
    /// nonce's bound principal differs (Agent vs Device).
    #[test]
    fn write_coord_mcp_agent_proxy_config_emits_agent_bound_loopback_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-aproxy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        let wd = tmp.to_string_lossy().to_string();
        let agent_id = uuid::Uuid::new_v4();

        write_coord_mcp_agent_proxy_config(&wd, 31337, agent_id);

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "http://127.0.0.1:31337/coord-mcp");
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("agent proxy config must carry the nonce header");
        assert!(!nonce.is_empty());
        assert!(
            server["headers"].get("Authorization").is_none(),
            "agent proxy shape must NOT bake a static bearer: {written}"
        );
        // The nonce is bound to THIS agent.
        assert_eq!(
            proxy_principal_for_nonce(nonce),
            Some(ProxyPrincipal::Agent { agent_id })
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// `coord_mcp_safe_to_write` — the non-clobber guard for the continuation
    /// provisioning path (continuations can degrade to a real repo checkout that
    /// already holds the operator's own `.mcp.json`).
    #[test]
    fn coord_mcp_safe_to_write_guards_user_config() {
        let dir = std::env::temp_dir().join(format!("coord-mcp-safe-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let wd = dir.to_string_lossy().to_string();
        let mcp = dir.join(".mcp.json");

        // 1. Absent → safe to create.
        assert!(coord_mcp_safe_to_write(&wd), "absent file must be writable");

        // 2. Solely our coord-mcp config → safe to refresh (we own it).
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"https://c/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a solely-coord-mcp config is ours — refreshable"
        );

        // 3. A user's own config (different server) → must NOT clobber.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"my-server":{"type":"http","url":"https://x/mcp"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "a foreign mcpServers config must be left untouched"
        );

        // 4. coord-mcp ALONGSIDE another server (2 keys) → not solely ours → skip.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"url":"https://c/mcp"},"other":{"url":"x"}}}"#,
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "coord-mcp plus a user server is the user's file — do not clobber"
        );

        // 5. Unparseable / non-JSON → conservatively do not clobber.
        std::fs::write(&mcp, "not json {{{").unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "an unparseable file must be left untouched"
        );

        // 6. Our coord-mcp config but bearer is an AGENT JWT → must NOT downgrade.
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        std::fs::write(
            &mcp,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            !coord_mcp_safe_to_write(&wd),
            "must not downgrade an existing agent-JWT coord-mcp config to a device JWT"
        );

        // 7. Same shape but a DEVICE-JWT bearer → still refreshable (ours, same tier).
        let device_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
        let device_jwt = format!("h.{device_payload}.s");
        std::fs::write(
            &mcp,
            format!(
                r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {device_jwt}"}}}}}}}}"#
            ),
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a device-JWT coord-mcp config is ours at the same tier — refreshable"
        );

        // 8. The PROXY shape (loopback URL + nonce header, no Authorization) is
        //    ours at the device tier — refreshable. This is what every
        //    device-provisioned session's `.mcp.json` now looks like.
        std::fs::write(
            &mcp,
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"abc123"}}}}"#,
        )
        .unwrap();
        assert!(
            coord_mcp_safe_to_write(&wd),
            "a proxy-shaped sole-coord-mcp config is ours — refreshable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orchestration of `provision_coord_mcp_with_jwt` — the exact write-decision
    /// body the gate-continuation path (`agent_runtime::run_continuation_terminal`)
    /// and every `acquire_for_terminal` chokepoint caller run once a bearer is
    /// resolved. Exercised through the JWT-injecting seam so it is deterministic
    /// regardless of whether the host has a real device JWT in its encrypted slot.
    /// Covers all four branches: device writes the device PROXY shape, agent
    /// writes the per-AGENT proxy shape (+ registers a live-token slot), a
    /// non-coord bearer is gated out, and a device bearer never downgrades an
    /// existing baked-agent-JWT config.
    #[test]
    fn provision_with_jwt_orchestration() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        // Build an unsigned JWT (`h.<payload>.s`) carrying just the `sub_type`
        // claim — all the device-arm orchestration inspects.
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
            format!("h.{payload}.s")
        };
        // An agent JWT additionally carries `sub` (= agent_id) + `exp` so the
        // converted agent arm can build the live-token slot.
        let mk_agent = |agent_id: uuid::Uuid| {
            let payload = URL_SAFE_NO_PAD.encode(
                format!(r#"{{"sub_type":"agent","sub":"{agent_id}","exp":9999999999}}"#).as_bytes(),
            );
            format!("h.{payload}.s")
        };
        let new_dir = || {
            let d = std::env::temp_dir().join(format!("coord-mcp-prov-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let mcp_of = |d: &std::path::Path| d.join(".mcp.json");
        let dev = mk("device");

        // A) device bearer + clean dir → provisions the loopback PROXY shape on
        //    the passed bound port: nonce header, NO static bearer.
        let d = new_dir();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, Some(19876));
        let written =
            std::fs::read_to_string(mcp_of(&d)).expect("device bearer must provision .mcp.json");
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(
            server["url"], "http://127.0.0.1:19876/coord-mcp",
            "device path must emit the loopback proxy URL on the passed port"
        );
        assert!(
            server["headers"]["X-Coord-Mcp-Proxy-Key"]
                .as_str()
                .is_some_and(|n| proxy_nonce_is_valid(n)),
            "device path must carry a registered per-session nonce"
        );
        assert!(
            server["headers"].get("Authorization").is_none(),
            "device path must NOT bake a static bearer (the proxy injects it live)"
        );
        let _ = std::fs::remove_dir_all(&d);

        // B) agent bearer + clean dir → provisions the per-AGENT PROXY shape
        //    (loopback URL + nonce, NO baked bearer) and registers the agent's
        //    live-token slot. The nonce is bound to THIS agent_id, and the proxy
        //    injects the agent's own refreshed token per request — never a static
        //    bearer, never the device token.
        let d = new_dir();
        let agent_id = uuid::Uuid::new_v4();
        let agent_jwt = mk_agent(agent_id);
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &agent_jwt, Some(19876));
        let written =
            std::fs::read_to_string(mcp_of(&d)).expect("agent bearer must provision .mcp.json");
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(
            server["url"], "http://127.0.0.1:19876/coord-mcp",
            "agent path now emits the per-agent loopback proxy URL"
        );
        assert!(
            server["headers"].get("Authorization").is_none(),
            "agent path must NOT bake a static bearer (the proxy injects it live)"
        );
        let nonce = server["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .expect("agent path must carry a per-session nonce");
        assert_eq!(
            proxy_principal_for_nonce(nonce),
            Some(ProxyPrincipal::Agent { agent_id }),
            "the nonce must be bound to this agent_id"
        );
        assert!(
            lookup_agent_token(agent_id).is_some(),
            "the agent arm must register a live-token slot"
        );
        remove_agent_token(agent_id);
        let _ = std::fs::remove_dir_all(&d);

        // C) non-coord bearer (e.g. a Cognito access token, sub_type=access) →
        //    sub_type gate skips; no file is written (would 401 coord's verifier).
        let d = new_dir();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &mk("access"), Some(19876));
        assert!(
            !mcp_of(&d).exists(),
            "a non-device/agent bearer must NOT be written"
        );
        let _ = std::fs::remove_dir_all(&d);

        // D) device bearer into a dir already holding an AGENT-JWT coord-mcp config →
        //    the no-downgrade guard vetoes; the agent config is preserved verbatim.
        let d = new_dir();
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {}"}}}}}}}}"#,
            mk("agent")
        );
        std::fs::write(mcp_of(&d), &agent_cfg).unwrap();
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, Some(19876));
        assert_eq!(
            std::fs::read_to_string(mcp_of(&d)).unwrap(),
            agent_cfg,
            "a device bearer must not downgrade an existing agent-JWT config"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3a — fail-closed: a DEVICE bearer with an UNKNOWN bound port
    /// (`None`) must NOT write a proxy `.mcp.json` (which would point at a dead
    /// bootstrap-default port), and must drop the degraded breadcrumb instead.
    /// This is the F1 root-cause guard.
    #[test]
    fn device_path_with_no_bound_port_writes_no_proxy_config() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-noport-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        // None bound port → fail-closed.
        provision_coord_mcp_with_jwt(&wd, &dev, None);

        assert!(
            !d.join(".mcp.json").exists(),
            "an unresolvable bound port must NOT write a (dead) proxy .mcp.json"
        );
        let crumb = std::fs::read_to_string(d.join(COORD_MCP_STATUS_FILE))
            .expect("a degraded breadcrumb must be written when the port is unresolvable");
        assert!(
            crumb.contains("coord-mcp UNREACHABLE") && crumb.contains("/gate"),
            "breadcrumb must be the actionable degraded line: {crumb}"
        );

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3a — a non-degraded healthy DEVICE provision (a real `Some(port)`)
    /// must NOT leave a degraded breadcrumb from the write path itself (the
    /// async probe may add one later if the port is dead, but the synchronous
    /// write must not).
    #[test]
    fn device_path_with_bound_port_writes_proxy_and_no_synchronous_breadcrumb() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let dev = {
            let payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"device"}"#);
            format!("h.{payload}.s")
        };
        let d = std::env::temp_dir().join(format!("coord-mcp-port-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&d).unwrap();
        let wd = d.to_string_lossy().to_string();

        provision_coord_mcp_with_jwt(&wd, &dev, Some(34567));

        assert!(
            d.join(".mcp.json").exists(),
            "a resolvable bound port must write the proxy .mcp.json"
        );
        let port = read_proxy_port(&wd);
        assert_eq!(port, Some(34567), "config must carry the passed bound port");

        let _ = std::fs::remove_dir_all(&d);
    }

    /// Phase 3b — persist→restore round-trip: a minted nonce is mirrored to the
    /// store, and `restore_proxy_nonces_from_store` re-loads it into a fresh
    /// in-memory map so it still validates after a (simulated) restart.
    #[test]
    fn persisted_nonce_survives_restore_round_trip() {
        // Inject a temp-dir store directly — NO process-global env mutation, so
        // this test cannot pollute sibling tests that read the default store.
        let (store_dir, store) = temp_store("nonce");

        // Mint a nonce in the live map, then mirror the snapshot to the INJECTED
        // store (the `register_proxy_nonce` body, split across its seams).
        let workdir = store_dir.join("session-wd").to_string_lossy().to_string();
        let (nonce, snapshot) = mint_and_register_nonce(&workdir, ProxyPrincipal::Device);
        persist_proxy_nonces_with_store(&store, &snapshot);
        assert!(proxy_nonce_is_valid(&nonce));

        // It is actually on disk (independent of the in-memory map).
        let persisted = store.load_coord_mcp_nonces();
        assert_eq!(
            persisted.get(&nonce).map(String::as_str),
            Some(workdir.as_str()),
            "the minted nonce must be mirrored to the encrypted store"
        );

        // Simulate a restart: drop the nonce from the in-memory map, then
        // restore from the injected store via the same merge the boot path runs.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&nonce);
        }
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "precondition: the nonce is gone from the in-memory map"
        );
        restore_proxy_nonces_from(&store);
        assert!(
            proxy_nonce_is_valid(&nonce),
            "a persisted nonce must validate again after a restore"
        );

        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// OQ3 — an AGENT nonce is NEVER mirrored to the persisted store, while a
    /// DEVICE nonce in the same snapshot still is. A restarted runner thus has
    /// no live agent session AND no restored agent nonce, so an agent nonce
    /// hard-fails closed across a restart.
    #[test]
    fn agent_nonce_is_not_persisted_device_nonce_is() {
        let (store_dir, store) = temp_store("agent-nonce");

        // Mint an AGENT nonce, then persist the snapshot to the injected store.
        let agent_id = uuid::Uuid::new_v4();
        let agent_wd = store_dir.join("agent-wd").to_string_lossy().to_string();
        let (agent_nonce, snapshot) =
            mint_and_register_nonce(&agent_wd, ProxyPrincipal::Agent { agent_id });
        persist_proxy_nonces_with_store(&store, &snapshot);

        // Also mint a DEVICE nonce and persist.
        let dev_wd = store_dir.join("dev-wd").to_string_lossy().to_string();
        let (dev_nonce, snapshot) = mint_and_register_nonce(&dev_wd, ProxyPrincipal::Device);
        persist_proxy_nonces_with_store(&store, &snapshot);

        let persisted = store.load_coord_mcp_nonces();
        assert!(
            !persisted.contains_key(&agent_nonce),
            "an agent nonce must NEVER be persisted to the encrypted store"
        );
        assert_eq!(
            persisted.get(&dev_nonce).map(String::as_str),
            Some(dev_wd.as_str()),
            "a device nonce in the same map must still be persisted"
        );

        // Cleanup the in-memory map entries.
        {
            let mut map = proxy_nonces().lock().unwrap();
            map.remove(&agent_nonce);
            map.remove(&dev_nonce);
        }
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    /// Phase 3b — `persist_proxy_nonces` honors the `nonce_persistence_enabled`
    /// gate: when persistence is disabled (test default, `COORD_MCP_PERSIST_NONCES`
    /// unset under `cfg(test)`), the DEFAULT-store path is a no-op. We assert the
    /// gate directly (a pure predicate) rather than mutating env + probing the
    /// real store — the injected-store seam makes the disk write deterministic and
    /// the gate is the only behavior worth pinning here.
    #[test]
    fn persistence_disabled_skips_default_store_write() {
        // Under cfg(test) with the env var unset, persistence is OFF by default
        // (see `nonce_persistence_enabled`), so `register_proxy_nonce`'s
        // default-store mirror is a guaranteed no-op — minting touches only the
        // in-memory map. A minted nonce is therefore valid in-memory but the
        // default store is never written. We verify the gate is the thing that
        // makes that true.
        assert!(
            !nonce_persistence_enabled(),
            "test builds must default persistence OFF so nonce-minting tests \
             never write the developer's real store"
        );
        // And minting still registers in-memory (the persist step is skipped).
        let (nonce, _snapshot) =
            mint_and_register_nonce("/tmp/coord-mcp-persist-off-wd", ProxyPrincipal::Device);
        assert!(
            proxy_nonce_is_valid(&nonce),
            "minting must register the nonce in-memory regardless of persistence"
        );
    }

    /// Phase 3c — the pure reconcile predicate: rewrite only when a proxy port
    /// is present AND differs from the current bound port.
    #[test]
    fn reconcile_action_rewrites_only_on_port_mismatch() {
        assert_eq!(
            reconcile_action(Some(9877), 9876),
            ReconcileAction::Rewrite,
            "stale port → rewrite"
        );
        assert_eq!(
            reconcile_action(Some(9876), 9876),
            ReconcileAction::Leave,
            "matching port → leave"
        );
        assert_eq!(
            reconcile_action(None, 9876),
            ReconcileAction::Leave,
            "no readable proxy port (absent / static-bearer agent config) → leave"
        );
    }

    /// Phase 3c — end-to-end reconcile over a workdir set: a stale-port proxy
    /// config is rewritten to the bound port; a matching one and an agent
    /// (static-bearer) config are left untouched.
    #[test]
    fn reconcile_session_configs_rewrites_stale_leaves_agent() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let base = std::env::temp_dir().join(format!("coord-mcp-recon-{}", uuid::Uuid::now_v7()));

        // Point the root-config self-heal (Phase 5b) at `base` — which holds no
        // `.mcp.json` — so this test exercises ONLY the session path and never
        // touches the operator's real `qontinui-root/.mcp.json`. Restored below.
        std::fs::create_dir_all(&base).unwrap();
        let prev_root = std::env::var("QONTINUI_ROOT").ok();
        std::env::set_var("QONTINUI_ROOT", &base);

        // Stale proxy config on :9999 → must be rewritten to :9876.
        let stale = base.join("stale");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(
            stale.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9999/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"old"}}}}"#,
        )
        .unwrap();

        // Already-correct proxy config on :9876 → must be left as-is.
        let ok = base.join("ok");
        std::fs::create_dir_all(&ok).unwrap();
        std::fs::write(
            ok.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"keep"}}}}"#,
        )
        .unwrap();

        // Agent static-bearer config → must NEVER be clobbered.
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
        );
        let agent = base.join("agent");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join(".mcp.json"), &agent_cfg).unwrap();

        let workdirs = vec![
            stale.to_string_lossy().to_string(),
            ok.to_string_lossy().to_string(),
            agent.to_string_lossy().to_string(),
        ];
        let rewritten = reconcile_session_configs(workdirs, 9876);
        assert_eq!(
            rewritten, 1,
            "only the stale-port proxy config is rewritten"
        );

        // Stale rewritten to the bound port (+ a fresh registered nonce).
        assert_eq!(read_proxy_port(&stale.to_string_lossy()), Some(9876));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(stale.join(".mcp.json")).unwrap())
                .unwrap();
        let new_nonce = v["mcpServers"]["coord-mcp"]["headers"]["X-Coord-Mcp-Proxy-Key"]
            .as_str()
            .unwrap();
        assert!(
            proxy_nonce_is_valid(new_nonce),
            "the rewritten config must carry a freshly-registered nonce"
        );

        // Correct config untouched; agent config preserved verbatim.
        assert_eq!(read_proxy_port(&ok.to_string_lossy()), Some(9876));
        assert_eq!(
            std::fs::read_to_string(agent.join(".mcp.json")).unwrap(),
            agent_cfg,
            "an agent static-bearer config must never be clobbered by the reconcile"
        );

        let _ = std::fs::remove_dir_all(&base);
        match prev_root {
            Some(p) => std::env::set_var("QONTINUI_ROOT", p),
            None => std::env::remove_var("QONTINUI_ROOT"),
        }
    }

    /// Phase 5b + plan 2026-07-07 Change 1 — the boot self-heal of the stale ROOT
    /// `.mcp.json` (the checked-in repo-root coord-mcp config). Drives the
    /// env-free `reconcile_root_config_at` so it neither mutates `QONTINUI_ROOT`
    /// nor touches the operator's real root config. Covers every dimension:
    /// wrong port (Rewrite), same-port-but-unregistered-nonce (ADOPT — Change 1
    /// core fix, file stays byte-identical), and the leave-alone cases (matching
    /// port + live nonce, absent file, foreign static-bearer).
    #[test]
    fn reconcile_root_config_self_heals_stale_root_mcp_json() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        // --- Case 1: stale PORT → Rewrite (client must reconnect anyway). ---
        let root = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9999/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"deadnonce"}}}}"#,
        )
        .unwrap();
        assert!(root_config_is_stale(&root, 9876), "wrong port is stale");
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::Rewrite,
            "a stale-port root config must be rewritten"
        );
        // Rewritten to the bound port with a freshly-registered nonce.
        assert_eq!(read_proxy_port(&root.to_string_lossy()), Some(9876));
        let new_nonce = read_proxy_nonce(&root.join(".mcp.json")).unwrap();
        assert!(
            proxy_nonce_is_valid(&new_nonce),
            "the rewritten root config must carry a freshly-registered nonce"
        );
        assert_ne!(new_nonce, "deadnonce");
        // And NO baked Authorization bearer (proxy shape).
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap())
                .unwrap();
        assert!(v["mcpServers"]["coord-mcp"]["headers"]
            .get("Authorization")
            .is_none());

        // --- Case 2: matching port + LIVE nonce → Leave (no rewrite). ---
        // The case-1 rewrite left a live nonce on port 9876; a second pass is a
        // no-op and must NOT mint a new nonce.
        assert!(
            !root_config_is_stale(&root, 9876),
            "matching port + live nonce is not stale"
        );
        assert_eq!(
            reconcile_root_config_at(&root, 9876),
            RootReconcileAction::Leave,
            "a fresh root config must not be touched again"
        );
        assert_eq!(
            read_proxy_nonce(&root.join(".mcp.json")).unwrap(),
            new_nonce
        );

        // --- Case 3: matching port but UNREGISTERED nonce → ADOPT (no rewrite). ---
        // Plan 2026-07-07 Change 1 CORE FIX: re-register the EXACT on-disk nonce
        // rather than minting a fresh one, so a live MCP client's cached nonce
        // keeps validating. The `.mcp.json` MUST stay byte-identical.
        let dead = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dead).unwrap();
        let dead_body = r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"notregistered-9c3f"}}}}"#;
        std::fs::write(dead.join(".mcp.json"), dead_body).unwrap();
        assert!(
            !proxy_nonce_is_valid("notregistered-9c3f"),
            "precondition: the on-disk nonce is not yet registered"
        );
        assert!(
            root_config_is_stale(&dead, 9876),
            "right port but an unregistered nonce needs healing"
        );
        assert_eq!(
            reconcile_root_config_at(&dead, 9876),
            RootReconcileAction::AdoptNonce,
            "an unregistered same-port nonce must be ADOPTED, not rewritten"
        );
        // The EXACT on-disk nonce is now valid (adopted verbatim into the registry).
        assert!(
            proxy_nonce_is_valid("notregistered-9c3f"),
            "the on-disk nonce must be adopted verbatim so a cached client validates"
        );
        // The file is byte-identical — no rewrite → live client cache preserved.
        assert_eq!(
            std::fs::read_to_string(dead.join(".mcp.json")).unwrap(),
            dead_body,
            "adopt must NOT rewrite the root .mcp.json"
        );
        // A second pass is now a no-op (the nonce is registered) → Leave.
        assert!(!root_config_is_stale(&dead, 9876));
        assert_eq!(
            reconcile_root_config_at(&dead, 9876),
            RootReconcileAction::Leave
        );

        // --- Case 4: absent root file → Leave. ---
        let empty = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            !root_config_is_stale(&empty, 9876),
            "absent file is not stale"
        );
        assert_eq!(
            reconcile_root_config_at(&empty, 9876),
            RootReconcileAction::Leave
        );
        assert!(!empty.join(".mcp.json").exists());

        // --- Case 5: foreign STATIC-BEARER (agent) root → never clobbered. ---
        let agent_payload = URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent_jwt = format!("h.{agent_payload}.s");
        let agent_cfg = format!(
            r#"{{"mcpServers":{{"coord-mcp":{{"type":"http","url":"https://c/mcp","headers":{{"Authorization":"Bearer {agent_jwt}"}}}}}}}}"#
        );
        let agent = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join(".mcp.json"), &agent_cfg).unwrap();
        // A static-bearer shape has no proxy URL → not flagged stale (and even if
        // it were, `coord_mcp_safe_to_write` would refuse the rewrite).
        assert!(!root_config_is_stale(&agent, 9876));
        assert_eq!(
            reconcile_root_config_at(&agent, 9876),
            RootReconcileAction::Leave
        );
        assert_eq!(
            std::fs::read_to_string(agent.join(".mcp.json")).unwrap(),
            agent_cfg,
            "a static-bearer root config must never be clobbered"
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&dead);
        let _ = std::fs::remove_dir_all(&empty);
        let _ = std::fs::remove_dir_all(&agent);
    }

    /// Plan 2026-07-07 Change 1 — the pure `root_reconcile_action` resolver over
    /// explicit inputs, isolating the adopt-vs-rewrite-vs-leave decision from any
    /// file I/O or the process-global nonce map.
    #[test]
    fn root_reconcile_action_resolves_adopt_vs_rewrite_vs_leave() {
        // Not our shape (no readable proxy port) → Leave regardless of nonce.
        assert_eq!(
            root_reconcile_action(None, Some("x"), false, 9876),
            RootReconcileAction::Leave
        );
        // Port moved → Rewrite (client's cached URL is stale too — reconnect).
        assert_eq!(
            root_reconcile_action(Some(9999), Some("x"), true, 9876),
            RootReconcileAction::Rewrite,
            "a moved port must rewrite even with a registered nonce"
        );
        // Same port, nonce readable but UNregistered → Adopt (the core fix).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), false, 9876),
            RootReconcileAction::AdoptNonce
        );
        // Same port, nonce readable AND registered → Leave (healthy).
        assert_eq!(
            root_reconcile_action(Some(9876), Some("abc"), true, 9876),
            RootReconcileAction::Leave
        );
        // Same port, NO nonce readable → Rewrite (nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), None, false, 9876),
            RootReconcileAction::Rewrite
        );
        // Same port, EMPTY nonce string → Rewrite (empty is nothing to adopt).
        assert_eq!(
            root_reconcile_action(Some(9876), Some(""), false, 9876),
            RootReconcileAction::Rewrite
        );
    }

    /// Plan 2026-07-07 Change 1 — `adopt_on_disk_nonce` re-registers the EXACT
    /// string as a live Device binding so a subsequent proxy request with that
    /// string is accepted (the gate's nonce check passes), WITHOUT minting a new
    /// nonce. Mirrors the restart-survival contract at the unit level.
    #[test]
    fn adopt_on_disk_nonce_reregisters_exact_string_as_device() {
        let workdir = format!("D:/adopt-wt-{}", uuid::Uuid::now_v7());
        let nonce = format!("ondisk-{}", uuid::Uuid::new_v4().simple());
        assert!(
            !proxy_nonce_is_valid(&nonce),
            "precondition: not registered"
        );

        adopt_on_disk_nonce(&workdir, &nonce);

        assert!(
            proxy_nonce_is_valid(&nonce),
            "the exact on-disk nonce must validate after adoption"
        );
        assert_eq!(
            proxy_principal_for_nonce(&nonce),
            Some(ProxyPrincipal::Device),
            "an adopted nonce is bound to the Device principal"
        );
    }

    /// Plan 2026-07-07 Change 3 — a DEVICE nonce evicted by a same-workdir
    /// re-mint stays valid through its grace TTL (so an in-flight client rides
    /// through), while the fresh nonce is live. An AGENT nonce is NEVER graced.
    #[test]
    fn remint_graces_evicted_device_nonce_but_never_agent() {
        // Device: mint A, then re-mint B for the SAME workdir → A graced, B live.
        let wd = format!("D:/grace-wt-{}", uuid::Uuid::now_v7());
        let a = register_proxy_nonce(&wd);
        assert!(proxy_nonce_is_valid(&a));
        let b = register_proxy_nonce(&wd);
        assert_ne!(a, b);
        assert!(proxy_nonce_is_valid(&b), "the fresh device nonce is live");
        assert!(
            proxy_nonce_is_valid(&a),
            "the evicted device nonce rides the grace TTL"
        );
        assert_eq!(
            proxy_principal_for_nonce(&a),
            Some(ProxyPrincipal::Device),
            "a graced nonce resolves to Device (no scope elevation)"
        );

        // Agent: mint A, re-mint B for the SAME workdir → A NOT graced (fails closed).
        let awd = format!("D:/grace-agent-wt-{}", uuid::Uuid::now_v7());
        let agent_id = uuid::Uuid::new_v4();
        let a2 = register_agent_proxy_nonce(&awd, agent_id);
        assert!(proxy_nonce_is_valid(&a2));
        let b2 = register_agent_proxy_nonce(&awd, agent_id);
        assert_ne!(a2, b2);
        assert!(proxy_nonce_is_valid(&b2), "the fresh agent nonce is live");
        assert!(
            !proxy_nonce_is_valid(&a2),
            "an evicted AGENT nonce must hard-fail closed — never graced"
        );
    }

    /// Plan 2026-07-07 Change 3 — grace fails closed exactly at the deadline:
    /// an entry whose `expires_at` is not strictly in the future is treated as
    /// expired and lazily evicted. Deterministic because monotonic time only
    /// advances — an entry stamped `now()` is never `> now()` at the later check.
    #[test]
    fn graced_nonce_expires_and_is_lazily_evicted() {
        let nonce = format!("expired-{}", uuid::Uuid::new_v4().simple());
        graced_nonces().lock().unwrap().insert(
            nonce.clone(),
            GracedNonce {
                expires_at: std::time::Instant::now(),
            },
        );
        assert!(
            !graced_nonce_is_valid(&nonce),
            "an already-elapsed grace entry must be invalid"
        );
        assert!(
            !graced_nonces().lock().unwrap().contains_key(&nonce),
            "an expired grace entry must be lazily evicted on the failing check"
        );
    }
}
