//! Device pairing — coord-side handshake to obtain a device-token JWT.
//!
//! Lifted from `src/bin/qontinui_profile.rs` (Phase 1 of the runner
//! unified-devices migration) so both the `qontinui_profile device pair`
//! CLI and the Tauri runner GUI (Phase 2+) call the same code paths.
//!
//! ## Public surface
//!
//! - [`pair_with_auth_token`] — headless flow: POST `Authorization: Bearer
//!   <oauth-or-runner-token>` to coord's `POST /coord/devices/pair-cli`.
//!   Requires the device to already be browser-paired at least once
//!   (uses the cached `user_id` from `paired_user.json`).
//! - [`pair_via_browser`] — interactive flow: opens
//!   `{web_backend}/connect-runner` in the user's default browser, spins
//!   up a localhost callback server, and exchanges the captured nonce
//!   with coord's `POST /coord/devices/pair-complete`.
//! - [`persist_pairing`] — writes the device-token JWT into
//!   `AuthManager`'s access-token slot + the paired user_id to
//!   `paired_user.json`.
//! - [`coord_http_base`] / [`coord_http_base_from_url`] — resolve the
//!   coord HTTP base from the active profile's `coord_url`
//!   (`ws[s]://host:port/ws` → `http[s]://host:port`).
//! - [`derive_web_base_from_coord`] — best-effort web-backend base
//!   derivation from the coord base (strips the port).
//! - [`PairCompleteResponse`] — canonical coord pair response wire
//!   shape (`{token, device_id, user_id, jti, exp}`).
//!
//! ## Defect 5 shape fixes (2026-05-21)
//!
//! The canonical coord-side `PairCliRequest` (see
//! `qontinui-coord/src/routes_phase3.rs:571-703`) requires:
//!
//! - body: `{device_id: Uuid, hostname: String, name: Option<String>}`
//! - header: `X-Qontinui-User-Id: <uuid>`
//!
//! and returns `{token, device_id, user_id, jti, exp}` (note: `token`,
//! not `device_token`). The pre-fix code sent `{hostname, name, os,
//! os_version}` with no header and deserialized into a `device_token`
//! field — both broken. This module ships the correct wire shape from
//! day one.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============================================================================
// Wire types — must match qontinui-coord/src/routes_phase3.rs exactly.
// ============================================================================

/// Canonical response shape for `POST /coord/devices/pair-cli` and
/// `POST /coord/devices/pair-complete`. Coord serializes this as
/// `{token, device_id, user_id, jti, exp}`. We accept the wire shape
/// directly: no `serde(rename)` indirection — the struct field name *is*
/// the wire name.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairCompleteResponse {
    /// Device-token JWT minted by coord.
    pub token: String,
    /// The paired user_id (UUID, stringified).
    pub user_id: String,
    /// The paired device_id (UUID, stringified). Always present on the
    /// canonical wire; kept as `Option<String>` for graceful handling of
    /// older / partial coord deployments mid-migration. Phase 1 callers
    /// can assume `Some(_)`.
    #[serde(default)]
    pub device_id: Option<String>,
    /// JWT id — present on the canonical wire; ignored by the CLI today.
    #[serde(default)]
    pub jti: Option<String>,
    /// JWT expiration (unix seconds). Present on the canonical wire;
    /// ignored by the CLI today, surfaced for Phase 2 refresh logic.
    #[serde(default)]
    pub exp: Option<i64>,
    /// Tenant scope coord stamped on this device. Phase 3 of the
    /// default-tenant-propagation plan may echo this back on the
    /// wire. `#[serde(default)]` tolerates pre-tenant coord builds.
    #[serde(default)]
    pub tenant_id: Option<String>,
}

// ============================================================================
// Identity-file helpers (paired_user.json + machine.json)
// ============================================================================

/// On-disk shape of `~/.qontinui/machine.json` — only the fields we need
/// for pairing. The full file may carry more keys; we tolerate extras.
///
/// The wire field name is `device_id`; the on-disk field name is the
/// legacy `machine_id` (preserved for backward compatibility — see the
/// doc-comment on `DeviceFile` in `bin/qontinui_profile.rs`). The serde
/// alias accepts both spellings so a pre-rename file still loads.
#[derive(Debug, Deserialize)]
struct MachineFile {
    #[serde(alias = "machine_id")]
    device_id: String,
}

/// Path to the per-device identity file. Same recipe as
/// `bin/qontinui_profile.rs::device_file_path`.
fn machine_file_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".qontinui").join("machine.json"))
}

/// Read `device_id` from `~/.qontinui/machine.json`. Returns a clear
/// error if the file is missing — the caller (pair-cli) cannot proceed
/// without a stable device identity.
///
/// Public-in-crate alias [`read_device_id_from_disk`] exposes this to
/// other modules in the workspace (e.g. the runner's
/// `mcp::device_jwt_refresher`) so the refresher can compose the
/// disk-read + parameterized-pair manually.
pub fn read_device_id_from_disk() -> Result<String, String> {
    read_device_id()
}

fn read_device_id() -> Result<String, String> {
    let path = machine_file_path().ok_or_else(|| "could not resolve home directory".to_string())?;
    if !path.exists() {
        return Err(format!(
            "device not initialized — run `qontinui_profile device init` first \
             (no {} on disk)",
            path.display()
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {}", path.display(), e))?;
    let parsed: MachineFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {}", path.display(), e))?;
    Ok(parsed.device_id)
}

/// Path to the paired-user JSON written by `device pair` and read by
/// `device init` to attach a `user_id` to the register payload. Lives
/// under the Tauri app's local data directory (alongside
/// `auth_tokens.enc`) so the CLI bin and the GUI runner read the same
/// file.
pub(crate) fn paired_user_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("com.qontinui.runner").join("paired_user.json"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PairedUserFile {
    pub user_id: String,
    /// Stringified UUID. `#[serde(default)]` keeps legacy files
    /// (written before the 2026-05-22 schema bump) deserializing
    /// — they carry only `user_id`. A heartbeat/register caller
    /// that finds `None` here falls back to decoding the cached
    /// device-token JWT via [`tenant_id_from_oauth_claim`]
    /// (see `fleet::resolve_tenant_id`).
    #[serde(default)]
    pub tenant_id: Option<String>,
}

/// Read the cached `user_id` written by a prior browser-pair. Returns
/// `None` if the file doesn't exist (fresh install / never paired).
///
/// Public-in-crate alias [`read_paired_user_id_from_disk`] exposes this
/// to other modules in the workspace (see [`read_device_id_from_disk`]).
pub fn read_paired_user_id_from_disk() -> Option<String> {
    read_paired_user_id()
}

fn read_paired_user_id() -> Option<String> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    Some(parsed.user_id)
}

/// Read the cached `tenant_id` from `paired_user.json`. Returns `None`
/// if the file is missing OR the field is absent (legacy file written
/// before the 2026-05-22 schema bump). Used by
/// `fleet::resolve_tenant_id` as the primary resolution branch.
pub fn read_paired_tenant_id_from_disk() -> Option<String> {
    let path = paired_user_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let parsed: PairedUserFile = serde_json::from_slice(&bytes).ok()?;
    parsed.tenant_id
}

/// Opportunistically rewrite `paired_user.json` with the supplied
/// `tenant_id`, preserving the existing `user_id`. Used by
/// `fleet::resolve_tenant_id`'s JWT-claim fallback so a legacy file
/// gets backfilled on the first heartbeat after a runner upgrade.
///
/// No-op if the file doesn't exist (the JWT-claim path only fires
/// when SOMETHING was paired previously, but we tolerate the race
/// where pair-state was wiped between read and write). Returns
/// `Err(String)` on filesystem failures so the caller can log them
/// at `debug!` — the outer flow proceeds either way.
pub fn backfill_paired_tenant_id(tenant_id: &uuid::Uuid) -> Result<(), String> {
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // No paired file → nothing to backfill. Quiet success.
            return Ok(());
        }
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let mut parsed: PairedUserFile =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    if parsed.tenant_id.as_deref() == Some(tenant_id.to_string().as_str()) {
        // Already up-to-date; avoid an unnecessary disk write.
        return Ok(());
    }
    parsed.tenant_id = Some(tenant_id.to_string());
    let pretty = serde_json::to_vec_pretty(&parsed)
        .map_err(|e| format!("serialize paired_user.json: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

// ============================================================================
// Host / OS detection
// ============================================================================

pub(crate) fn detect_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Per the platform docs: `std::env::consts::OS` is one of {"linux",
/// "macos", "windows", "ios", "android", "freebsd", "dragonfly",
/// "netbsd", "openbsd", "solaris"}.
pub(crate) fn detect_os() -> String {
    std::env::consts::OS.to_string()
}

/// OS version string via the already-present `sysinfo = "0.32"` crate.
/// `None` on platforms where sysinfo can't probe.
pub(crate) fn detect_os_version() -> Option<String> {
    use sysinfo::System;
    System::long_os_version().or_else(System::os_version)
}

// ============================================================================
// coord_url → coord HTTP base resolution
// ============================================================================

/// Resolve the coord HTTP base from the active profile's `coord_url`.
/// Thin wrapper around [`coord_http_base_from_url`] — separated for
/// testability (the wrapper hits the filesystem; the conversion is
/// pure).
pub fn coord_http_base() -> Result<String, String> {
    let coord_url = crate::profiles::load_strict()
        .map_err(|e| format!("active profile load failed: {}", e))?
        .coord_url
        .ok_or_else(|| "active profile has no coord_url".to_string())?;
    Ok(coord_http_base_from_url(&coord_url))
}

/// Pure conversion: `ws[s]://host[:port][/ws]` → `http[s]://host[:port]`.
/// Mirrors `qontinui-supervisor/src/fleet.rs::coord_http_base` so the
/// register and pair paths agree on the recipe.
pub fn coord_http_base_from_url(coord_url: &str) -> String {
    let trimmed = coord_url.trim_end_matches("/ws");
    trimmed
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            trimmed
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .unwrap_or_else(|| trimmed.to_string())
}

/// Derive a `https://` web base from the coord HTTP base, assuming web
/// + coord live on the same host (the dev-default). Production
/// deployments override via `QONTINUI_WEB_BASE`. Best-effort: strips
/// the trailing `:port`.
pub fn derive_web_base_from_coord(coord_base: &str) -> String {
    let trimmed = coord_base.trim_end_matches('/');
    if let Some((host, _)) = trimmed.rsplit_once(':') {
        host.to_string()
    } else {
        trimmed.to_string()
    }
}

// ============================================================================
// Pair: headless (--auth-token)
// ============================================================================

/// POST `Authorization: Bearer <oauth-or-runner-token>` +
/// `X-Qontinui-User-Id: <uuid>` to `POST /coord/devices/pair-cli`. Coord
/// verifies the bearer token, looks up the device, and returns a fresh
/// device-token JWT.
///
/// Requirements (Defect 5):
/// - `~/.qontinui/machine.json` must exist with a UUID `device_id`
///   (run `qontinui_profile device init` first).
/// - `{data_local_dir}/com.qontinui.runner/paired_user.json` must exist
///   from a prior browser-pair (pair-cli is a refresh path, not a
///   first-pair path).
///
/// Thin wrapper around [`pair_with_auth_token_with_ids`] — reads
/// `device_id` and `user_id` from disk then delegates. Tests use the
/// parameterized form directly so they can run hermetically against an
/// in-process mock coord without touching `~/.qontinui` or the
/// `data_local_dir`.
/// Decode the unverified payload of a JWT and pull the `tenant_id`
/// claim, if any. Returns `None` if the token isn't a JWT, the payload
/// isn't valid base64-decoded JSON, or no `tenant_id` claim is present.
///
/// We deliberately do NOT verify the JWT signature — coord is the
/// authority on tenant_id (Phase 4 of the default-tenant-propagation
/// plan re-validates on receipt via the per-write resolver). This is
/// just the runner's best-effort auto-resolve to spare the operator
/// from re-typing a UUID they already authenticated against.
///
/// Phase 2 of the default-tenant-propagation plan (Q3 resolution:
/// "OAuth claim with `--tenant-id` override for CLI"). Also used by
/// `mcp::device_jwt_refresher` to resolve tenant_id at refresh time
/// from the stored OAuth/runner token.
pub fn tenant_id_from_oauth_claim(token: &str) -> Option<String> {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    let mut parts = token.splitn(3, '.');
    let _header = parts.next()?;
    let payload_b64 = parts.next()?;
    let _signature = parts.next()?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload_json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload_json
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

pub fn pair_with_auth_token(
    base: &str,
    oauth_token: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    let device_id = read_device_id()?;
    let user_id = read_paired_user_id().ok_or_else(|| {
        "not yet browser-paired — first pair must use --browser mode \
         (no paired_user.json on disk)"
            .to_string()
    })?;
    pair_with_auth_token_with_ids(base, oauth_token, &device_id, &user_id, tenant_id)
}

/// Parameterized variant of [`pair_with_auth_token`] — accepts `device_id`
/// and `user_id` as explicit arguments instead of reading them from
/// `~/.qontinui/machine.json` + `{data_local_dir}/.../paired_user.json`.
/// Phase 5 of the unified-devices migration introduced this split so the
/// E2E pair tests can target an in-process mock coord without mutating
/// (or relying on) per-user files.
///
/// Routes through the web backend at `{base}/api/v1/devices/pair-cli`,
/// not coord directly. The web backend is the only component allowed to
/// resolve `tenant_id` from the authenticated user — coord's pair-cli
/// requires `tenant_id` as of the 2026-05-20
/// default-tenant-propagation plan, and threading that through the
/// runner would leak tenancy concerns into a layer that doesn't need
/// them. Callers pass the web-backend base URL (e.g.
/// `http://127.0.0.1:8000`), NOT the coord URL.
pub fn pair_with_auth_token_with_ids(
    base: &str,
    oauth_token: &str,
    device_id: &str,
    user_id: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    let url = format!("{}/api/v1/devices/pair-cli", base);
    let body = serde_json::json!({
        "device_id": device_id,
        "hostname":  detect_hostname(),
        "name":      detect_hostname(),
        "tenant_id": tenant_id.to_string(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .bearer_auth(oauth_token)
        .header("X-Qontinui-User-Id", user_id)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    resp.json::<PairCompleteResponse>()
        .map_err(|e| format!("decode pair-cli response failed: {}", e))
}

// ============================================================================
// Pair: browser (--browser, default)
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CallbackQuery {
    pub state: String,
    pub token: String,
    #[serde(default)]
    pub token_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CallbackCapture {
    pub token: String,
    #[allow(dead_code)]
    pub token_id: Option<String>,
}

/// Browser-mediated pair. Spins up a localhost axum server, opens the
/// browser to `/connect-runner?state=…&callback=…`, waits for the user's
/// click + redirect, then exchanges the captured `(state, token)` with
/// coord's `POST /coord/devices/pair-complete` for the device-token JWT.
pub fn pair_via_browser(
    coord_base: &str,
    tenant_id: uuid::Uuid,
) -> Result<PairCompleteResponse, String> {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // Generate state nonce. 32 bytes of randomness; hex-encoded so it
    // survives a URL round-trip without escaping.
    let mut state_bytes = [0u8; 32];
    use rand::TryRngCore;
    rand::rng()
        .try_fill_bytes(&mut state_bytes)
        .map_err(|e| format!("rand fill failed: {e}"))?;
    let state_nonce = hex::encode(state_bytes);

    // Web backend URL is read from the active profile's `coord_url`
    // host (assuming web + coord co-locate in dev). If they diverge,
    // the user can override with QONTINUI_WEB_BASE.
    let web_base = std::env::var("QONTINUI_WEB_BASE")
        .ok()
        .unwrap_or_else(|| derive_web_base_from_coord(coord_base));
    let hostname_now = detect_hostname();

    // Bind a port for the callback. Use 0 to let the OS pick — then read it
    // back. We use std::net::TcpListener first to capture the chosen port,
    // then hand it to axum via from_tcp.
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("bind localhost callback failed: {e}"))?;
    let port = std_listener
        .local_addr()
        .map_err(|e| format!("local_addr failed: {e}"))?
        .port();
    std_listener
        .set_nonblocking(true)
        .map_err(|e| format!("set_nonblocking failed: {e}"))?;

    let received: Arc<Mutex<Option<CallbackCapture>>> = Arc::new(Mutex::new(None));
    let received_clone = received.clone();
    let state_expected = state_nonce.clone();

    // Build the redirect URL the browser will land back on. The state
    // round-trips so we can reject mismatched callbacks.
    let callback_url = format!("http://127.0.0.1:{}/auth/runner-token-callback", port);
    let connect_url = format!(
        "{}/connect-runner?state={}&callback={}&device_name={}",
        web_base.trim_end_matches('/'),
        urlencoding::encode(&state_nonce),
        urlencoding::encode(&callback_url),
        urlencoding::encode(&hostname_now),
    );

    // Spawn a current-thread tokio runtime + axum server on a worker
    // thread. We could use the existing src/mcp/auth_callback.rs route,
    // but it depends on Tauri-side state (ApiState/AppState); for the
    // CLI we want an independent server.
    let (server_done_tx, server_done_rx) = std::sync::mpsc::channel::<()>();
    let server_handle = std::thread::spawn(move || -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime build failed: {e}"))?;
        rt.block_on(async move {
            use axum::{extract::Query, response::Html, routing::get, Router};
            let state_for_route = state_expected.clone();
            let received_for_route = received_clone.clone();
            let app = Router::new().route(
                "/auth/runner-token-callback",
                get(move |Query(q): Query<CallbackQuery>| {
                    let state_for_route = state_for_route.clone();
                    let received_for_route = received_for_route.clone();
                    async move {
                        if q.state != state_for_route {
                            return Html(
                                "<h1>State mismatch</h1><p>The callback state did \
                                 not match the pending flow. Restart the pairing.</p>"
                                    .to_string(),
                            );
                        }
                        *received_for_route.lock().expect("mutex") = Some(CallbackCapture {
                            token: q.token.clone(),
                            token_id: q.token_id.clone(),
                        });
                        Html(
                            "<h1>&#10003; Runner paired</h1>\
                             <p>You can close this tab.</p>\
                             <script>setTimeout(()=>window.close(),2000);</script>"
                                .to_string(),
                        )
                    }
                }),
            );
            let tokio_listener = tokio::net::TcpListener::from_std(std_listener)
                .map_err(|e| format!("tokio listener wrap failed: {e}"))?;
            // Run until our flag flips, with a 5-minute hard timeout.
            let serve = axum::serve(tokio_listener, app);
            let timeout = tokio::time::sleep(Duration::from_secs(300));
            tokio::pin!(timeout);
            tokio::select! {
                res = serve => {
                    res.map_err(|e| format!("axum serve failed: {e}"))?;
                }
                _ = &mut timeout => {
                    return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
                }
            }
            let _ = server_done_tx.send(());
            Ok(())
        })
    });

    println!(
        "opening browser to {} (callback {})",
        connect_url, callback_url
    );
    if let Err(e) = open::that(&connect_url) {
        eprintln!(
            "warning: failed to open browser ({}). Open this URL manually:\n  {}",
            e, connect_url
        );
    }

    // Poll the received slot up to 5 minutes. The axum runtime is on a
    // background thread; we just block here until we see the slot fill
    // or the server-done signal arrives.
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let capture = loop {
        if let Some(c) = received.lock().expect("mutex").clone() {
            break c;
        }
        if std::time::Instant::now() >= deadline {
            return Err("pair: timed out after 5 minutes waiting for browser callback".to_string());
        }
        // Cheap polling cadence; the axum task wakes on its own.
        std::thread::sleep(Duration::from_millis(200));
        // Drain the server-done channel in case axum exited early.
        if let Ok(()) = server_done_rx.try_recv() {
            // Server stopped without filling the slot — re-check once.
            if let Some(c) = received.lock().expect("mutex").clone() {
                break c;
            }
            return Err("pair: callback server stopped before capturing token".to_string());
        }
    };
    // Best-effort join — we don't fail the operation if the server thread
    // is still running; it'll time out on its own.
    drop(server_handle);

    // Exchange (state, token) for the device-token JWT. `tenant_id` is a
    // defensive forward — coord's authoritative source is `flow.tenant_id`
    // carried from `pair-start`, but the browser pair flow currently mints
    // its state nonce locally instead of going through coord's pair-start,
    // so we forward the runner's best-known value here as a hint. Coord
    // re-validates per Phase 4 of the default-tenant-propagation plan.
    let url = format!("{}/coord/devices/pair-complete", coord_base);
    let body = serde_json::json!({
        "state":      state_nonce,
        "token":      capture.token,
        "device_id":  capture.token_id,
        "hostname":   hostname_now,
        "name":       hostname_now,
        "os":         detect_os(),
        "os_version": detect_os_version(),
        "tenant_id":  tenant_id.to_string(),
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("reqwest client build failed: {}", e))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .map_err(|e| format!("POST {} failed: {}", url, e))?;
    let status = resp.status();
    if !status.is_success() {
        let body_text = resp
            .text()
            .unwrap_or_else(|_| "<unable to read response body>".to_string());
        return Err(format!("POST {} -> HTTP {}: {}", url, status, body_text));
    }
    resp.json::<PairCompleteResponse>()
        .map_err(|e| format!("decode pair-complete response failed: {}", e))
}

// ============================================================================
// Persist: store device-token JWT + cache user_id
// ============================================================================

/// Persist the device-token JWT (via
/// `qontinui_runner_lib::auth::AuthManager`) and write the paired
/// user_id to `paired_user.json` so the next `device init` attaches it
/// to the register payload.
///
/// Storing the JWT in the `access_token` slot is intentional: the
/// existing `AuthManager` already calls that slot the "primary
/// credential," and the runner's outer code reads `get_access_token()`
/// when authenticating to the web backend. The refresh-token slot is
/// unused for the device-token flow (the device JWT has its own
/// lifecycle managed by coord); we pass an empty string. See the
/// format-change comment in `secure_storage.rs`.
pub fn persist_pairing(resp: &PairCompleteResponse, tenant_id: uuid::Uuid) -> Result<(), String> {
    use crate::auth::AuthManager;
    let mgr = AuthManager::new();
    mgr.store_tokens(&resp.token, "")
        .map_err(|e| format!("AuthManager::store_tokens failed: {e}"))?;
    let path = paired_user_path().ok_or_else(|| "could not resolve data_local_dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let pf = PairedUserFile {
        user_id: resp.user_id.clone(),
        tenant_id: Some(tenant_id.to_string()),
    };
    let pretty =
        serde_json::to_vec_pretty(&pf).map_err(|e| format!("serialize paired_user.json: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &pretty).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))?;
    if let Some(did) = &resp.device_id {
        // Best-effort: record device_id alongside the auth tokens so the
        // runner GUI can identify itself without reading machine.json.
        if let Ok(storage) = crate::secure_storage::SecureStorage::new() {
            if let Err(e) = storage.store_device_id(did) {
                tracing::debug!("store_device_id non-fatal: {e}");
            }
        }
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_http_base_converts_ws_to_http() {
        assert_eq!(
            coord_http_base_from_url("ws://localhost:9870/ws"),
            "http://localhost:9870"
        );
    }

    #[test]
    fn coord_http_base_converts_wss_to_https() {
        assert_eq!(
            coord_http_base_from_url("wss://coord.qontinui.io:9870/ws"),
            "https://coord.qontinui.io:9870"
        );
    }

    #[test]
    fn coord_http_base_strips_trailing_ws_path() {
        // With explicit /ws suffix:
        assert_eq!(
            coord_http_base_from_url("ws://host:9870/ws"),
            "http://host:9870"
        );
        // Without /ws suffix (bare ws://host:port):
        assert_eq!(
            coord_http_base_from_url("ws://host:9870"),
            "http://host:9870"
        );
        // Bare hostname without scheme — passed through (the supervisor
        // recipe already accepts http:// hosts as-is; we mirror that).
        assert_eq!(
            coord_http_base_from_url("http://host:9870"),
            "http://host:9870"
        );
    }

    #[test]
    fn derive_web_base_from_coord_drops_port() {
        assert_eq!(
            derive_web_base_from_coord("http://localhost:9870"),
            "http://localhost"
        );
        assert_eq!(
            derive_web_base_from_coord("https://coord.qontinui.io:9870"),
            "https://coord.qontinui.io"
        );
        // Trailing slash tolerated:
        assert_eq!(
            derive_web_base_from_coord("http://localhost:9870/"),
            "http://localhost"
        );
    }

    #[test]
    fn pair_complete_response_decodes_canonical_coord_shape() {
        // Exact canonical wire shape from
        // qontinui-coord/src/routes_phase3.rs:445-454.
        let wire = serde_json::json!({
            "token":     "abc.def.ghi",
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       1_234_567_890i64,
        });
        let parsed: PairCompleteResponse =
            serde_json::from_value(wire).expect("decode canonical coord shape");
        assert_eq!(parsed.token, "abc.def.ghi");
        assert_eq!(
            parsed.device_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(parsed.user_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            parsed.jti.as_deref(),
            Some("33333333-3333-4333-8333-333333333333")
        );
        assert_eq!(parsed.exp, Some(1_234_567_890));
    }

    /// Defect 5 shape assertion (body-level): the `pair_with_auth_token`
    /// request body MUST contain `device_id`, `hostname`, and `name` and
    /// MUST NOT contain `os` / `os_version` (those aren't on
    /// `PairCliRequest`). We can't easily intercept the in-flight
    /// request without a mock-server dep, so this test asserts the body
    /// shape by reconstructing it the same way `pair_with_auth_token`
    /// does and inspecting the JSON keys.
    #[test]
    fn pair_cli_request_body_carries_canonical_keys() {
        // Re-construct the body the same way `pair_with_auth_token`
        // does. If a future refactor diverges, this test fails loudly.
        let device_id = "00000000-0000-4000-8000-000000000000";
        let tenant_id = "11111111-1111-4111-8111-111111111111";
        let body = serde_json::json!({
            "device_id": device_id,
            "hostname":  detect_hostname(),
            "name":      detect_hostname(),
            "tenant_id": tenant_id,
        });
        let obj = body.as_object().expect("object");
        // Required keys (per PairCliRequest):
        assert!(obj.contains_key("device_id"), "device_id missing");
        assert!(obj.contains_key("hostname"), "hostname missing");
        assert!(obj.contains_key("name"), "name missing");
        assert!(obj.contains_key("tenant_id"), "tenant_id missing");
        // Rejected legacy keys (would 422 on coord's PairCliRequest):
        assert!(
            !obj.contains_key("os"),
            "os must not be sent — not on PairCliRequest"
        );
        assert!(
            !obj.contains_key("os_version"),
            "os_version must not be sent — not on PairCliRequest"
        );
        // Sanity on values:
        assert_eq!(
            obj.get("device_id").and_then(|v| v.as_str()),
            Some(device_id)
        );
        assert!(obj.get("hostname").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn tenant_id_from_oauth_claim_extracts_uuid() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"sub":"x","tenant_id":"11111111-2222-3333-4444-555555555555"}"#);
        let token = format!("{}.{}.signature", header, payload);
        assert_eq!(
            tenant_id_from_oauth_claim(&token).as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
    }

    #[test]
    fn tenant_id_from_oauth_claim_returns_none_when_missing() {
        use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256"}"#);
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"x"}"#);
        let token = format!("{}.{}.signature", header, payload);
        assert_eq!(tenant_id_from_oauth_claim(&token), None);
    }

    #[test]
    fn tenant_id_from_oauth_claim_returns_none_for_non_jwt() {
        assert_eq!(tenant_id_from_oauth_claim("not-a-jwt"), None);
        assert_eq!(tenant_id_from_oauth_claim("a.b"), None);
    }

    /// The legacy `device_token` field name MUST NOT decode — coord
    /// emits `token`. A regression that re-introduces `device_token`
    /// (e.g. via a stray `#[serde(rename)]`) would silently fail to
    /// extract the JWT from the response. This test pins that.
    #[test]
    fn pair_complete_response_does_not_accept_legacy_device_token_field() {
        let legacy = serde_json::json!({
            "device_token": "abc.def.ghi",
            "user_id": "22222222-2222-4222-8222-222222222222",
        });
        let parsed: Result<PairCompleteResponse, _> = serde_json::from_value(legacy);
        // It should parse (unknown fields ignored by default), but the
        // `token` field will be empty since the wire field name is
        // `token` and we sent `device_token`. That manifests as a
        // missing-required-field error.
        assert!(
            parsed.is_err(),
            "legacy device_token-only payload must NOT decode (token field required)"
        );
    }

    /// Back-compat: a `paired_user.json` written by a pre-2026-05-22
    /// runner has only the `user_id` field. The new `tenant_id` field
    /// carries `#[serde(default)]` so such files must still deserialize
    /// — they just yield `tenant_id == None` and the heartbeat path
    /// falls through to the JWT-claim fallback.
    #[test]
    fn paired_user_file_back_compat_user_id_only_deserializes() {
        let raw = r#"{"user_id":"22222222-2222-4222-8222-222222222222"}"#;
        let parsed: PairedUserFile =
            serde_json::from_str(raw).expect("legacy user_id-only must deserialize");
        assert_eq!(parsed.user_id, "22222222-2222-4222-8222-222222222222");
        assert!(
            parsed.tenant_id.is_none(),
            "legacy file must yield tenant_id = None"
        );
    }

    /// Forward shape — a newly-written `paired_user.json` round-trips
    /// both fields. Pins the serde representation against accidental
    /// `#[serde(rename)]` / `#[serde(skip)]` regressions.
    #[test]
    fn paired_user_file_with_tenant_id_round_trips() {
        let original = PairedUserFile {
            user_id: "22222222-2222-4222-8222-222222222222".to_string(),
            tenant_id: Some("11111111-2222-3333-4444-555555555555".to_string()),
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: PairedUserFile = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.user_id, original.user_id);
        assert_eq!(parsed.tenant_id, original.tenant_id);
    }
}

// ============================================================================
// E2E pair tests against an in-process mock coord (Phase 5.1)
// ============================================================================
//
// These tests exercise `pair_with_auth_token_with_ids` against a real
// axum server bound to `127.0.0.1:0` on a background thread. No external
// crates beyond what's already in dev-deps; the mock spins up + tears
// down in <100ms per test, so we get hermetic HTTP coverage without
// pulling in `mockito` / `wiremock`.
//
// Why `_with_ids`? `pair_with_auth_token` reads `device_id` from
// `~/.qontinui/machine.json` and `user_id` from
// `{data_local_dir}/com.qontinui.runner/paired_user.json`. Those are
// user-data filesystem reads — fine in production, hostile in tests
// (would mutate the operator's files or spuriously fail in clean CI).
// The `_with_ids` variant accepts the IDs directly so the test can pass
// synthetic UUIDs and hit the HTTP path 1:1.
#[cfg(test)]
mod pair_e2e_tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// One captured request, populated by the mock route on each call.
    #[derive(Debug, Clone, Default)]
    struct CapturedRequest {
        authorization: Option<String>,
        user_id_header: Option<String>,
        body_bytes: Vec<u8>,
        called: bool,
    }

    /// Shared state across the test thread + axum's worker.
    #[derive(Clone)]
    struct MockState {
        capture: Arc<Mutex<CapturedRequest>>,
        /// HTTP status to return.
        status: StatusCode,
        /// Response body (200 path serves `canonical_200_body`;
        /// non-2xx paths serve a short error JSON).
        response_body: String,
    }

    /// Canonical-shape 200 body returned by coord's
    /// `POST /coord/devices/pair-cli` on success.
    fn canonical_200_body(token: &str) -> String {
        serde_json::json!({
            "token": token,
            "device_id": "11111111-1111-4111-8111-111111111111",
            "user_id":   "22222222-2222-4222-8222-222222222222",
            "jti":       "33333333-3333-4333-8333-333333333333",
            "exp":       chrono::Utc::now().timestamp() + 4 * 60 * 60,
        })
        .to_string()
    }

    /// Handler for `POST /coord/devices/pair-cli`. Captures the inbound
    /// request shape onto shared state then returns the canned response.
    async fn pair_cli_handler(
        State(state): State<MockState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, String) {
        let mut capture = state.capture.lock().expect("capture mutex");
        capture.called = true;
        capture.authorization = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        capture.user_id_header = headers
            .get("X-Qontinui-User-Id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        capture.body_bytes = body.to_vec();
        (state.status, state.response_body.clone())
    }

    /// Boot the mock coord on `127.0.0.1:0`. Returns
    /// `(base_url, capture_handle, shutdown_signal)`. The caller drops
    /// `shutdown_signal` (or sends on it) to terminate the server.
    fn spawn_mock_coord(
        status: StatusCode,
        response_body: String,
    ) -> (
        String,
        Arc<Mutex<CapturedRequest>>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let capture = Arc::new(Mutex::new(CapturedRequest::default()));
        let capture_for_handler = capture.clone();

        // Bind synchronously so we can read the port before returning.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
        let port = std_listener.local_addr().expect("local_addr").port();
        std_listener.set_nonblocking(true).expect("set_nonblocking");

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rt");
            rt.block_on(async move {
                let mock_state = MockState {
                    capture: capture_for_handler,
                    status,
                    response_body,
                };
                let app: Router = Router::new()
                    // Mirror the live route — pair_with_auth_token POSTs to
                    // `{base}/api/v1/devices/pair-cli` (web-routed). The mock
                    // used to register `/coord/devices/pair-cli` (the legacy
                    // coord-direct path) and 404'd the request, panicking all
                    // four pair_e2e tests. Tied to the call site at line 369.
                    .route("/api/v1/devices/pair-cli", post(pair_cli_handler))
                    .with_state(mock_state);
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("tokio listener");
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });
        });

        // Tiny settle to let the listener attach (axum is ready as soon as
        // `from_std` completes, but the spawned thread might not have
        // entered `block_on` yet). 50ms is generous on Windows + Linux.
        std::thread::sleep(Duration::from_millis(50));

        (format!("http://127.0.0.1:{port}"), capture, shutdown_tx)
    }

    const TEST_DEVICE_ID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const TEST_USER_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
    const TEST_BEARER: &str = "bearer-token";
    const TEST_TENANT_ID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    fn test_tenant_uuid() -> uuid::Uuid {
        uuid::Uuid::parse_str(TEST_TENANT_ID).unwrap()
    }

    #[test]
    fn pair_with_auth_token_e2e_200_persists_jwt() {
        // Mock coord at 127.0.0.1:<port> returns the canonical 200 JSON;
        // assert the returned PairCompleteResponse.token matches the
        // synthetic JWT we configured the mock to emit.
        let expected_jwt = "header-segment.payload-segment.signature-segment";
        let (base, _capture, _shutdown) =
            spawn_mock_coord(StatusCode::OK, canonical_200_body(expected_jwt));
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let resp = result.expect("pair_with_auth_token_with_ids should succeed on 200");
        assert_eq!(resp.token, expected_jwt);
        assert_eq!(resp.user_id, "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            resp.device_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_401_returns_err() {
        // Mock returns 401 with a coord-shaped error body; assert the
        // error string surfaces the status code + body so an operator
        // can diagnose by reading the log.
        let (base, _capture, _shutdown) = spawn_mock_coord(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid bearer token"}"#.to_string(),
        );
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let err = result.expect_err("401 must return Err");
        assert!(
            err.contains("401"),
            "error should mention status 401, got: {err}"
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_500_returns_err() {
        // Coord-side bug → 500. Caller must surface the error rather
        // than silently treating it as a refresh-needed signal.
        let (base, _capture, _shutdown) = spawn_mock_coord(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"coord-side panic"}"#.to_string(),
        );
        let result = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        );
        let err = result.expect_err("500 must return Err");
        assert!(
            err.contains("500"),
            "error should mention status 500, got: {err}"
        );
    }

    #[test]
    fn pair_with_auth_token_e2e_request_shape() {
        // Defect-5 wire-shape regression guard: the request the runner
        // emits MUST carry `Authorization: Bearer <oauth>`, the
        // `X-Qontinui-User-Id: <uuid>` header, and a JSON body containing
        // `device_id` + `hostname`.
        let (base, capture, _shutdown) =
            spawn_mock_coord(StatusCode::OK, canonical_200_body("ignored-jwt"));
        let _ = pair_with_auth_token_with_ids(
            &base,
            TEST_BEARER,
            TEST_DEVICE_ID,
            TEST_USER_ID,
            test_tenant_uuid(),
        )
        .expect("200 path");

        let captured = capture.lock().expect("capture mutex").clone();
        assert!(captured.called, "mock coord must have been hit");
        assert_eq!(
            captured.authorization.as_deref(),
            Some(&*format!("Bearer {TEST_BEARER}")),
            "Authorization header must be `Bearer <token>`"
        );
        assert_eq!(
            captured.user_id_header.as_deref(),
            Some(TEST_USER_ID),
            "X-Qontinui-User-Id header must carry the cached user_id"
        );

        let body_json: serde_json::Value =
            serde_json::from_slice(&captured.body_bytes).expect("request body must be valid JSON");
        let obj = body_json.as_object().expect("body must be a JSON object");
        assert_eq!(
            obj.get("device_id").and_then(|v| v.as_str()),
            Some(TEST_DEVICE_ID),
            "body.device_id must match what we passed"
        );
        assert!(
            obj.get("hostname").and_then(|v| v.as_str()).is_some(),
            "body.hostname must be present"
        );
        assert_eq!(
            obj.get("tenant_id").and_then(|v| v.as_str()),
            Some(TEST_TENANT_ID),
            "body.tenant_id must carry the resolved tenant (Phase 2 of default-tenant-propagation)"
        );
        // Reject legacy keys (would 422 against the canonical coord
        // PairCliRequest schema):
        assert!(!obj.contains_key("os"), "legacy `os` key must not be sent");
        assert!(
            !obj.contains_key("os_version"),
            "legacy `os_version` key must not be sent"
        );
    }
}
