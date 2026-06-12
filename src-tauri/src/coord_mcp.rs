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
//! in-memory map — it dies with the runner, and every spawn path rewrites
//! `.mcp.json`, so a restart self-heals.
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
/// nonce → workdir it was provisioned into. Process-lifetime only by design
/// (resolved Q2 of the plan): every spawn path rewrites `.mcp.json`, so a
/// runner restart re-mints nonces and self-heals.
static PROXY_NONCES: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn proxy_nonces() -> &'static Mutex<HashMap<String, String>> {
    PROXY_NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mint + register a fresh per-session proxy nonce for `workdir`, returning it.
/// Any prior nonce registered for the same workdir is evicted — a re-provision
/// rewrites `.mcp.json`, so the old nonce is unreachable and keeping it would
/// only widen the accept set.
fn register_proxy_nonce(workdir: &str) -> String {
    // Two v4 UUIDs (~244 bits of randomness) — v4, NOT v7: the v7 prefix is a
    // timestamp, which would gut the entropy this nonce exists to provide.
    let nonce = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut map = proxy_nonces().lock().expect("proxy nonce map poisoned");
    map.retain(|_, wd| wd != workdir);
    map.insert(nonce.clone(), workdir.to_string());
    nonce
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
/// `AppHandle`, falling back to [`crate::mcp::types::get_mcp_api_port`] only
/// when no Tauri runtime / managed state exists (headless or unit-test
/// contexts that genuinely predate the bind). The env-default 9876 fallback is
/// wrong on secondary/temp runners, which is why the bound port is preferred.
pub(crate) fn resolve_bound_api_port() -> u16 {
    if let Some(app) = crate::tauri_app_handle::current() {
        use tauri::Manager;
        if let Some(state) = app.try_state::<std::sync::Arc<crate::commands::AppState>>() {
            return crate::mcp::types::runner_api_port(state.inner());
        }
    }
    crate::mcp::types::get_mcp_api_port()
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
pub(crate) fn provision_coord_mcp_for_session(workdir: &str, bound_port: u16) {
    let jwt = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.trim().is_empty() => t,
        _ => {
            info!(
                "coord_mcp: no device JWT in access_token slot — skipping \
                 coord-mcp provisioning for {workdir}"
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
fn provision_coord_mcp_with_jwt(workdir: &str, jwt: &str, bound_port: u16) {
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
        write_coord_mcp_proxy_config(workdir, bound_port);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, 19876);
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
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &agent_jwt, 19876);
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
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &mk("access"), 19876);
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
        provision_coord_mcp_with_jwt(&d.to_string_lossy(), &dev, 19876);
        assert_eq!(
            std::fs::read_to_string(mcp_of(&d)).unwrap(),
            agent_cfg,
            "a device bearer must not downgrade an existing agent-JWT config"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
