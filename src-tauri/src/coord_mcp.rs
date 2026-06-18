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
//! coord-minted `SubType::Agent` JWT. The proxy attaches the live DEVICE JWT,
//! so ONLY device-provisioned sessions may route through it — the agent path
//! keeps the static-bearer shape, and the proxy gate re-checks `sub_type ==
//! "device"` on every request.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use tracing::{info, warn};

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
    // localhost fallback for the `.mcp.json` write is applied by the caller
    // (`write_coord_mcp_config`), unchanged.
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
/// nonce → workdir it was provisioned into. Mirrored to the encrypted local
/// store (Phase 3b) when `COORD_MCP_PERSIST_NONCES` is not `0`, so the set
/// survives a runner rebuild/restart and an already-written `.mcp.json` keeps
/// validating (the MCP client never re-reads the file). With persistence
/// disabled this degrades to the prior process-lifetime-only behavior.
static PROXY_NONCES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Guards [`restore_proxy_nonces_from_store`] so a second boot-restore (e.g. an
/// idempotent auto-start re-invocation) never re-loads over live in-memory
/// nonces minted since the first restore.
static PROXY_NONCES_RESTORED: OnceLock<()> = OnceLock::new();

fn proxy_nonces() -> &'static Mutex<HashMap<String, String>> {
    PROXY_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
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
fn persist_proxy_nonces(map: &HashMap<String, String>) {
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
    map: &HashMap<String, String>,
) {
    if let Err(e) = store.store_coord_mcp_nonces(map) {
        warn!("coord_mcp: failed to persist proxy nonces: {e}");
    }
}

/// Restore persisted proxy nonces into the in-memory registry on boot (Phase
/// 3b). Idempotent + run-once: merges the persisted set UNDER any nonces already
/// minted this process (live mints win on key collision, which cannot happen in
/// practice — the persisted set predates this process). No-op when persistence
/// is disabled. Wire this into the same startup path as the other auto-start
/// tasks so already-written `.mcp.json` nonces keep validating post-restart.
pub(crate) fn restore_proxy_nonces_from_store() {
    if !nonce_persistence_enabled() {
        return;
    }
    if PROXY_NONCES_RESTORED.set(()).is_err() {
        return; // already restored once this process
    }
    let store = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s,
        Err(e) => {
            warn!("coord_mcp: secure storage unavailable, cannot restore proxy nonces: {e}");
            return;
        }
    };
    restore_proxy_nonces_from(&store);
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
            map.entry(nonce).or_insert(workdir);
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
    let (nonce, snapshot) = mint_and_register_nonce(workdir);
    persist_proxy_nonces(&snapshot);
    nonce
}

/// Mint a fresh nonce, evict any prior nonce for `workdir`, insert it, and
/// return `(nonce, snapshot)` — WITHOUT persisting. Split from the persistence
/// step so a test can mint and then mirror to an INJECTED store
/// ([`persist_proxy_nonces_with_store`]) instead of the default store reached
/// via the process-global `QONTINUI_SECURE_STORAGE_DIR`.
fn mint_and_register_nonce(workdir: &str) -> (String, HashMap<String, String>) {
    // Two v4 UUIDs (~244 bits of randomness) — v4, NOT v7: the v7 prefix is a
    // timestamp, which would gut the entropy this nonce exists to provide.
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let snapshot = {
        let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.retain(|_, wd| wd != workdir);
        map.insert(nonce.clone(), workdir.to_string());
        map.clone()
    };
    (nonce, snapshot)
}

/// True iff `nonce` is a currently-registered per-session proxy key.
pub(crate) fn proxy_nonce_is_valid(nonce: &str) -> bool {
    !nonce.is_empty()
        && proxy_nonces()
            .lock()
            .expect("proxy nonce map poisoned")
            .contains_key(nonce)
}

/// Pre-forward gate for the loopback `/coord-mcp` proxy route. Pure over its
/// inputs (no I/O) so the 401 paths are unit-testable without a live coord:
///
/// 1. `nonce` must be a registered per-session key ([`proxy_nonce_is_valid`])
///    — stops other local processes borrowing the runner's coord identity.
/// 2. `bearer` (the live `AuthManager` access-token read) must be present and
///    decode `sub_type == "device"` — the same guard as
///    [`provision_coord_mcp_with_jwt`], re-applied per request so the proxy
///    can never attach a non-device token (scope-elevation trap: agent
///    sessions carry their own narrower JWT and never route through here).
///
/// `Ok(())` means: forward with `Authorization: Bearer <bearer>`.
pub(crate) fn proxy_request_gate(
    nonce: Option<&str>,
    bearer: Option<&str>,
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
                "runner has no live device JWT in its access_token slot".to_string(),
            ));
        }
    };
    match jwt_unverified_claim(bearer, "sub_type").as_deref() {
        Some("device") => Ok(()),
        other => Err((
            401,
            format!(
                "runner access_token bearer is not a coord DEVICE JWT \
                 (sub_type={other:?}) — refusing to forward"
            ),
        )),
    }
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

/// The coord-native `/mcp` endpoint is live (coord PR #277 Phase-2 cutover),
/// so this no longer carries the prior "do not deploy" gate. If a target coord
/// ever lacks the `/mcp` route, agents get an unreachable MCP server: Claude
/// Code degrades gracefully and runs *without* coord tools (a silent
/// coordination regression) — so always sequence a coord `/mcp` deploy ahead
/// of pointing runners at a new coord.
///
/// Coord base resolution: `COORD_HTTP_URL` env → active profile's `coord_url`
/// → localhost fallback. Previously this read ONLY the env var, so a
/// profile-configured runner (production → `coord.qontinui.io`) wrote a
/// `localhost` MCP url into the agent's `.mcp.json`, silently pointing spawned
/// agents at the wrong coord.
pub(crate) fn write_coord_mcp_config(primary_wt: &str, jwt: &str) {
    let mcp_config = serde_json::json!({
        "mcpServers": {
            "coord-mcp": {
                "type": "http",
                "url": coord_mcp_url(),
                "headers": {
                    "Authorization": format!("Bearer {}", jwt),
                }
            }
        }
    });
    write_mcp_json(primary_wt, &mcp_config);
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

/// Filename of the Phase-1a degraded-only breadcrumb dropped into a session
/// workdir when coord-mcp provisioning is degraded (no JWT, unresolvable port,
/// or a failed reachability probe). Referenced by the `/gate` skill + CLAUDE.md.
pub(crate) const COORD_MCP_STATUS_FILE: &str = ".coord-mcp-status";

/// Write a SHORT, single-line degraded breadcrumb into `workdir` (Phase 1a).
/// Emitted ONLY when coord-mcp is degraded — a healthy session writes nothing
/// (the file is removed by [`clear_degraded_breadcrumb`] on a successful probe),
/// so its mere presence is the signal. Best-effort: a write failure only logs.
fn write_degraded_breadcrumb(workdir: &str, reason: &str) {
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
fn probe_and_breadcrumb_proxy(workdir: &str, port: u16) {
    // Resolve the nonce we just wrote for this workdir so the probe authenticates
    // exactly as the session's MCP client will.
    let nonce = {
        let map = proxy_nonces().lock().expect("proxy nonce map poisoned");
        map.iter()
            .find(|(_, wd)| wd.as_str() == workdir)
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
        write_coord_mcp_config(workdir, jwt);
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

/// True iff the root `.mcp.json` at `root_dir` holds a coord-mcp PROXY config
/// that is STALE for the current `bound_port` — its proxy port differs OR its
/// nonce is not in the live registry. Either case 401s/misroutes a spawned
/// agent that inherits the root file, so it must be rewritten. Returns `false`
/// (leave it) for an absent file, a non-proxy (static-bearer) shape, or a
/// proxy config whose port matches AND whose nonce is currently registered.
///
/// Split out as a pure-over-its-inputs predicate so the self-heal decision is
/// unit-testable without env or the process-global nonce map mutation order.
fn root_config_is_stale(root_dir: &Path, bound_port: u16) -> bool {
    let path = root_dir.join(".mcp.json");
    let Some(port) = read_proxy_port(&root_dir.to_string_lossy()) else {
        // Absent, unparseable, or a static-bearer (agent) shape — not ours to
        // refresh here. (The session reconcile's `coord_mcp_safe_to_write`
        // guard owns the agent-config-protection contract; we never touch one.)
        return false;
    };
    if port != bound_port {
        return true;
    }
    // Port matches: the only remaining staleness is an evicted/unrestored nonce
    // the live proxy would 401.
    match read_proxy_nonce(&path) {
        Some(nonce) => !proxy_nonce_is_valid(&nonce),
        None => true,
    }
}

/// Boot-time self-heal of the CHECKED-IN repo-root `.mcp.json` (Phase 5b),
/// resolving the root dir from the environment. Delegates to
/// [`reconcile_root_config_at`] — split so the rewrite is unit-testable against
/// an explicit temp dir WITHOUT mutating the process-global `QONTINUI_ROOT`
/// (the module deliberately avoids global-env mutation; see the test helpers).
fn reconcile_root_config(bound_port: u16) -> bool {
    match qontinui_root_dir() {
        Some(root_dir) => reconcile_root_config_at(&root_dir, bound_port),
        None => false,
    }
}

/// Self-heal the repo-root `.mcp.json` under `root_dir` (Phase 5b). The root
/// config is the loopback PROXY shape (port + per-session nonce); a spawned
/// agent that inherits it (cwd up the tree, no per-worktree config) breaks if
/// the nonce was evicted or the port moved across a restart/instance change.
/// When [`root_config_is_stale`] flags it, rewrite via the SAME
/// [`write_coord_mcp_proxy_config`] helper the session path uses (fresh
/// registered nonce + the current bound port) — guarded by
/// [`coord_mcp_safe_to_write`] so a hand-rolled static-bearer root file is never
/// clobbered. Returns `true` iff the root file was rewritten.
fn reconcile_root_config_at(root_dir: &Path, bound_port: u16) -> bool {
    if !root_config_is_stale(root_dir, bound_port) {
        return false;
    }
    let root = root_dir.to_string_lossy().to_string();
    if !coord_mcp_safe_to_write(&root) {
        return false;
    }
    write_coord_mcp_proxy_config(&root, bound_port);
    info!("coord_mcp: boot self-heal rewrote root {root}/.mcp.json to bound port :{bound_port}");
    true
}

/// Boot-time session-config reconcile (Phase 3c) + root-config self-heal
/// (Phase 5b). For each live session workdir, if its `.mcp.json` coord-mcp proxy
/// port ≠ the instance's CURRENT bound port, rewrite it via
/// [`write_coord_mcp_proxy_config`] (correct port + a freshly persisted nonce),
/// guarded by [`coord_mcp_safe_to_write`] so it never clobbers an agent-spawn's
/// static-bearer config. Combined with Phase 3b (persisted nonces), the common
/// same-port restart needs no rewrite at all — this covers only the
/// instance/port-change case. ALSO self-heals the checked-in repo-root
/// `.mcp.json` (see [`reconcile_root_config`]) so a spawned agent inheriting the
/// root file is never broken by an evicted nonce / stale port. Returns the
/// number of configs rewritten (sessions + the root file).
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
    // Belt-and-braces: self-heal the checked-in repo-root config too (Phase 5b).
    if reconcile_root_config(bound_port) {
        rewritten += 1;
    }
    if rewritten > 0 {
        info!("coord_mcp: boot reconcile rewrote {rewritten} config(s) to the current bound port");
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn write_coord_mcp_config_emits_http_bearer_shape() {
        let tmp = std::env::temp_dir().join(format!("coord-mcp-cfg-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&tmp).unwrap();
        let primary_wt = tmp.to_string_lossy().to_string();

        let prev = std::env::var("COORD_HTTP_URL").ok();
        std::env::set_var("COORD_HTTP_URL", "https://coord.example.test/");

        write_coord_mcp_config(&primary_wt, "header.payload.sig");

        let written = std::fs::read_to_string(tmp.join(".mcp.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];

        // HTTP transport pointing at coord /mcp, Bearer-authenticated.
        assert_eq!(server["type"], "http");
        assert_eq!(server["url"], "https://coord.example.test/mcp");
        assert_eq!(
            server["headers"]["Authorization"],
            "Bearer header.payload.sig"
        );

        // No Node-sidecar/subprocess residue, and no identity env vars
        // (identity is derived server-side from the JWT claims).
        assert!(server.get("command").is_none(), "must not spawn a command");
        assert!(server.get("args").is_none(), "must not pass node args");
        assert!(server.get("env").is_none(), "identity must come from JWT");
        assert!(
            !written.contains("node") && !written.contains("coord-mcp.mjs"),
            "config must not reference the Node sidecar: {written}"
        );

        // Cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
        match prev {
            Some(p) => std::env::set_var("COORD_HTTP_URL", p),
            None => std::env::remove_var("COORD_HTTP_URL"),
        }
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

        // Re-provisioning the same workdir evicts the prior nonce.
        write_coord_mcp_proxy_config(&primary_wt, 23456);
        assert!(
            !proxy_nonce_is_valid(nonce),
            "a re-provision must evict the prior nonce for the same workdir"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The loopback proxy's pre-forward gate: registered nonce + device bearer
    /// → forward; everything else → 401 before any network I/O. This is the
    /// scope-elevation backstop — even a valid nonce must never forward a
    /// non-DEVICE bearer.
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

        // Registered nonce + device bearer → forward.
        assert!(proxy_request_gate(Some(&nonce), Some(&device)).is_ok());

        // Absent / mismatched nonce → 401, regardless of bearer.
        assert_eq!(proxy_request_gate(None, Some(&device)).unwrap_err().0, 401);
        assert_eq!(
            proxy_request_gate(Some("not-a-registered-nonce"), Some(&device))
                .unwrap_err()
                .0,
            401
        );
        assert_eq!(
            proxy_request_gate(Some(""), Some(&device)).unwrap_err().0,
            401
        );

        // Valid nonce but no/empty bearer → 401.
        assert_eq!(proxy_request_gate(Some(&nonce), None).unwrap_err().0, 401);
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(" ")).unwrap_err().0,
            401
        );

        // Valid nonce but an AGENT bearer → 401 (scope-elevation trap: the
        // proxy must only ever attach the runner's DEVICE identity).
        let agent_err = proxy_request_gate(Some(&nonce), Some(&mk("agent"))).unwrap_err();
        assert_eq!(agent_err.0, 401);
        // A non-coord (e.g. Cognito) bearer → 401 too.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some(&mk("access")))
                .unwrap_err()
                .0,
            401
        );
        // A non-JWT string → 401, never a panic.
        assert_eq!(
            proxy_request_gate(Some(&nonce), Some("not-a-jwt"))
                .unwrap_err()
                .0,
            401
        );
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
    /// Covers all four branches: device writes the PROXY shape, agent writes the
    /// static-bearer shape, a non-coord bearer is gated out, and a device bearer
    /// never downgrades an existing agent config.
    #[test]
    fn provision_with_jwt_orchestration() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        // Build an unsigned JWT (`h.<payload>.s`) carrying just the `sub_type`
        // claim — all the orchestration inspects.
        let mk = |sub_type: &str| {
            let payload =
                URL_SAFE_NO_PAD.encode(format!(r#"{{"sub_type":"{sub_type}"}}"#).as_bytes());
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

        // B) agent bearer + clean dir → provisions the static-bearer shape,
        //    UNCHANGED (agent JWTs are narrower than the device JWT the proxy
        //    injects — agent sessions must never route through the proxy).
        let d = new_dir();
        let agent_jwt = mk("agent");
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &agent_jwt, Some(19876));
        let written =
            std::fs::read_to_string(mcp_of(&d)).expect("agent bearer must provision .mcp.json");
        let v: serde_json::Value = serde_json::from_str(&written).unwrap();
        let server = &v["mcpServers"]["coord-mcp"];
        assert_eq!(
            server["headers"]["Authorization"],
            format!("Bearer {agent_jwt}"),
            "agent path keeps the static bearer shape"
        );
        assert!(
            server["url"]
                .as_str()
                .is_some_and(|u| !u.contains("/coord-mcp")),
            "agent path must point at coord /mcp directly, never the proxy"
        );
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
        let (nonce, snapshot) = mint_and_register_nonce(&workdir);
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
        let (nonce, _snapshot) = mint_and_register_nonce("/tmp/coord-mcp-persist-off-wd");
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

    /// Phase 5b — the boot self-heal rewrites a stale ROOT `.mcp.json` (the
    /// checked-in repo-root coord-mcp config) to the current bound port + a
    /// freshly-registered nonce, so a spawned agent inheriting the root file is
    /// never broken by an evicted nonce / stale port. Drives the env-free
    /// `reconcile_root_config_at` so it neither mutates `QONTINUI_ROOT` nor
    /// touches the operator's real root config. Covers all three staleness
    /// dimensions: wrong port, dead nonce (right port), and the leave-alone
    /// cases (matching port + live nonce, absent file, foreign static-bearer).
    #[test]
    fn reconcile_root_config_self_heals_stale_root_mcp_json() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

        // --- Case 1: stale PORT → rewrite. ---
        let root = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9999/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"deadnonce"}}}}"#,
        )
        .unwrap();
        assert!(root_config_is_stale(&root, 9876), "wrong port is stale");
        assert!(
            reconcile_root_config_at(&root, 9876),
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

        // --- Case 2: matching port + LIVE nonce → leave (no rewrite). ---
        // The case-1 rewrite left a live nonce on port 9876; a second pass is a
        // no-op and must NOT mint a new nonce.
        assert!(
            !root_config_is_stale(&root, 9876),
            "matching port + live nonce is not stale"
        );
        assert!(
            !reconcile_root_config_at(&root, 9876),
            "a fresh root config must not be rewritten again"
        );
        assert_eq!(
            read_proxy_nonce(&root.join(".mcp.json")).unwrap(),
            new_nonce
        );

        // --- Case 3: matching port but DEAD nonce → rewrite. ---
        let dead = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dead).unwrap();
        std::fs::write(
            dead.join(".mcp.json"),
            r#"{"mcpServers":{"coord-mcp":{"type":"http","url":"http://127.0.0.1:9876/coord-mcp","headers":{"X-Coord-Mcp-Proxy-Key":"notregistered"}}}}"#,
        )
        .unwrap();
        assert!(
            root_config_is_stale(&dead, 9876),
            "right port but an unregistered nonce is stale"
        );
        assert!(reconcile_root_config_at(&dead, 9876));
        assert!(proxy_nonce_is_valid(
            &read_proxy_nonce(&dead.join(".mcp.json")).unwrap()
        ));

        // --- Case 4: absent root file → leave. ---
        let empty = std::env::temp_dir().join(format!("coord-mcp-root-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(
            !root_config_is_stale(&empty, 9876),
            "absent file is not stale"
        );
        assert!(!reconcile_root_config_at(&empty, 9876));
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
        assert!(!reconcile_root_config_at(&agent, 9876));
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
}
