//! `coord doctor` — one-command runner self-check for coord access + gate
//! registration (plan 2026-06-13 Phase 4).
//!
//! Runs SEVEN ordered checks, STOPS at the first red, and reports that link +
//! its fix. Green on all seven ⇒ "this runner can set gates." The output is
//! copy-pasteable and **identical across machines**, so MSI / spaceship / a
//! fresh box all self-diagnose the same way.
//!
//! # Why this lives in the lib crate
//!
//! Two consumers need it: the headless standalone bin (`src/bin/coord_doctor.rs`)
//! and the in-app Tauri command (`crate::coord_doctor_cmd` in the runner
//! binary). A `src/bin/*` crate cannot import the runner binary's module tree,
//! so the reusable core (types + the first-red-stops DRIVER + the report
//! FORMATTER + `diagnose`) lives here in `qontinui_runner_lib` where both can
//! reach it.
//!
//! The checks reuse the same on-disk + secure-storage state the live predicates
//! read (`auth`, `pair`, `secure_storage`, `profiles` are all lib modules that
//! compile into the runner binary too), so the bin and the Tauri command
//! produce the SAME report. The one fact only the running runtime knows — the
//! ACTUALLY-BOUND API port (check 6) — is injected via [`DoctorInputs`]: the
//! Tauri command passes the live `coord_mcp::resolve_bound_api_port()`, the
//! standalone bin passes `None` (and check 6 honestly reports "bound port
//! unknown — run from inside the running runner").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;

// ===========================================================================
// Result types (pure data — serialize straight to the Tauri command / JSON)
// ===========================================================================

/// One check's outcome. `name` identifies the link; `ok` is the verdict;
/// `detail` is a one-line human explanation; `fix` is the actionable next step
/// (only meaningful when `!ok`, but always carried so the report is uniform).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub fix: String,
}

/// The whole self-check: the ordered checks actually RUN (the driver stops
/// after the first red, so later checks are absent), plus the overall verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
    pub overall_ok: bool,
}

impl DoctorReport {
    /// Render the copy-pasteable, machine-identical text report. The ordering
    /// + the trailing verdict line are the contract — keep them stable so the
    /// output diffs cleanly across machines.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("coord doctor — runner gate-access self-check\n");
        out.push_str("============================================\n");
        for (i, c) in self.checks.iter().enumerate() {
            let mark = if c.ok { "PASS" } else { "FAIL" };
            out.push_str(&format!(
                "{}. [{}] {} — {}\n",
                i + 1,
                mark,
                c.name,
                c.detail
            ));
            if !c.ok {
                out.push_str(&format!("      fix: {}\n", c.fix));
            }
        }
        out.push_str("--------------------------------------------\n");
        if self.overall_ok {
            out.push_str("OK: this runner can set gates.\n");
        } else {
            // The first (only) red is the one to fix; name it again so a
            // glance at the last line is enough.
            let first_red = self.checks.iter().find(|c| !c.ok);
            match first_red {
                Some(c) => out.push_str(&format!("BLOCKED at: {} — {}\n", c.name, c.fix)),
                None => out.push_str("BLOCKED (no check ran).\n"),
            }
        }
        out
    }
}

// ===========================================================================
// Pure first-red-stops driver (injectable check fns → unit-testable)
// ===========================================================================

/// A single check: a static name + fix, plus a thunk that runs the predicate
/// and returns `(ok, detail)`. The thunk is `FnOnce` so a check can own
/// resources; the driver only ever calls it once.
pub struct Check<'a> {
    pub name: &'static str,
    pub fix: &'static str,
    pub run: Box<dyn FnOnce() -> (bool, String) + 'a>,
}

impl<'a> Check<'a> {
    pub fn new(
        name: &'static str,
        fix: &'static str,
        run: impl FnOnce() -> (bool, String) + 'a,
    ) -> Self {
        Self {
            name,
            fix,
            run: Box::new(run),
        }
    }
}

/// Run `checks` in order, STOPPING at (and including) the first red. Returns a
/// [`DoctorReport`] whose `checks` are exactly the ones that ran. Pure over the
/// injected closures — the unit tests drive it with fake checks to assert the
/// ordering / first-red-stops behavior without any live runner.
pub fn run_checks(checks: Vec<Check<'_>>) -> DoctorReport {
    let mut results = Vec::with_capacity(checks.len());
    let mut overall_ok = true;
    for check in checks {
        let name = check.name;
        let fix = check.fix;
        let (ok, detail) = (check.run)();
        results.push(CheckResult {
            name: name.to_string(),
            ok,
            detail,
            fix: fix.to_string(),
        });
        if !ok {
            overall_ok = false;
            break; // first red stops the chain
        }
    }
    DoctorReport {
        checks: results,
        overall_ok,
    }
}

// ===========================================================================
// Real wiring — the 7 checks, reusing existing predicates / on-disk state.
// ===========================================================================

/// Runtime-only facts the lib can't observe on its own. Injected so the Tauri
/// command (running inside the live runner) and the standalone bin produce the
/// same report shape, differing only where the bin genuinely cannot know the
/// live bound port.
#[derive(Debug, Clone, Default)]
pub struct DoctorInputs {
    /// The runner's ACTUALLY-BOUND loopback API port, from the live managed
    /// `AppState` (`coord_mcp::resolve_bound_api_port()`). `None` from the
    /// standalone bin (no running runtime) ⇒ check 6 reports the port as
    /// unverifiable rather than guessing.
    pub bound_api_port: Option<u16>,
    /// Optional explicit Claude config dir to check for check 1; `None` means
    /// "the ambient default location" (matches the spawn path's behavior).
    pub claude_config_dir: Option<String>,
}

/// Run the full 7-check self-check and return the structured report.
///
/// Each check reuses the canonical state:
/// 1. Claude account — `.credentials.json` validity (same paths + expiry logic
///    as `ai_provider::oauth_refresh::default_location_has_valid_credentials`).
/// 2. Tier — `settings.json::tier == "qontinui_account"`
///    (`settings::load_settings().tier == RunnerTier::QontinuiAccount`).
/// 3. Paired + signed in — `paired_user.json`
///    (`pair::read_paired_user_id_from_disk`) + a bearer in the access-token
///    slot (`auth::AuthManager::get_access_token`).
/// 4. Tenant resolvable — OAuth claim → outgoing-JWT claim →
///    `machine.json::active_tenant_id` (mirrors
///    `device_jwt_refresher::resolve_pair_tenant_id`).
/// 5. Device JWT live — `auth::AuthManager::device_jwt_needs_refresh()` ==
///    `Ok(false)` with a token present.
/// 6. `.mcp.json` valid — its coord-mcp port == the bound port AND its nonce
///    is registered + the bearer is a device JWT (mirrors
///    `coord_mcp::proxy_request_gate`).
/// 7. Coord reachable — a one-shot `tools/list` round-trips 200 against the
///    configured coord-mcp endpoint.
pub fn diagnose(inputs: &DoctorInputs) -> DoctorReport {
    let cfg_dir = inputs.claude_config_dir.clone();
    let bound_port = inputs.bound_api_port;

    let checks = vec![
        Check::new("claude_account", "run /login", move || {
            let ok = claude_account_has_valid_credentials(cfg_dir.as_deref());
            if ok {
                (
                    true,
                    "a Claude account with live credentials is present".into(),
                )
            } else {
                (
                    false,
                    "no authenticated Claude account (no valid ~/.claude/.credentials.json)".into(),
                )
            }
        }),
        Check::new("tier", "set runner tier to Qontinui account", || {
            let tier = read_runner_tier();
            if tier.as_deref() == Some("qontinui_account") {
                (true, "runner tier is Qontinui account".into())
            } else {
                (
                    false,
                    format!(
                        "runner tier is {} (not qontinui_account)",
                        tier.as_deref().unwrap_or("local")
                    ),
                )
            }
        }),
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new("paired_signed_in", "sign in / re-pair", move || {
                let paired = crate::pair::read_paired_user_id_from_disk().is_some();
                let bearer = auth_ref
                    .get_access_token()
                    .ok()
                    .filter(|t| !t.trim().is_empty())
                    .is_some();
                match (paired, bearer) {
                    (true, true) => (
                        true,
                        "paired_user.json present and a bearer is stored".into(),
                    ),
                    (false, _) => (false, "paired_user.json missing — runner not paired".into()),
                    (true, false) => (
                        false,
                        "paired but access-token slot is empty — sign in".into(),
                    ),
                }
            })
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(
                "tenant_resolvable",
                "machine.json missing active_tenant_id",
                move || {
                    let bearer = auth_ref.get_access_token().ok().unwrap_or_default();
                    match resolve_tenant_for_doctor(&bearer) {
                        Some((tid, src)) => (true, format!("tenant {tid} resolved from {src}")),
                        None => (
                            false,
                            "no tenant_id from OAuth claim, outgoing device-JWT, or \
                             machine.json::active_tenant_id"
                                .into(),
                        ),
                    }
                },
            )
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(
                DEVICE_JWT_LIVE_CHECK_NAME,
                DEVICE_JWT_LIVE_CHECK_FIX,
                move || device_jwt_live_predicate(&auth_ref),
            )
        },
        {
            let auth_ref = crate::auth::AuthManager::new();
            Check::new(
                "mcp_json_valid",
                "stale config — reprovision",
                move || mcp_json_check(bound_port, &auth_ref),
            )
        },
        Check::new(
            "coord_reachable",
            "coord unreachable",
            coord_reachable_check,
        ),
    ];

    run_checks(checks)
}

// ===========================================================================
// Check 5 (device JWT live) — extracted so the Phase-5 provisioning gate can
// reuse the EXACT predicate `diagnose` runs (no second device-JWT impl).
// ===========================================================================

/// The stable `name`/`fix` for check 5, shared between [`diagnose`] and
/// [`device_jwt_live_check`] so the provisioning gate and the full doctor
/// report name the link identically.
pub const DEVICE_JWT_LIVE_CHECK_NAME: &str = "device_jwt_live";
pub const DEVICE_JWT_LIVE_CHECK_FIX: &str = "kick refresher / re-pair";

/// The check-5 predicate over an `AuthManager`: a live device JWT is present
/// and not near expiry. `(ok, detail)`. The single source of truth for "does
/// this runner hold a usable coord device JWT" — both the full [`diagnose`]
/// chain and the Phase-5 provisioning gate call it.
fn device_jwt_live_predicate(auth: &crate::auth::AuthManager) -> (bool, String) {
    match auth.device_jwt_needs_refresh() {
        Ok(false) => {
            if auth
                .get_access_token()
                .ok()
                .filter(|t| !t.trim().is_empty())
                .is_some()
            {
                (true, "device JWT present and not near expiry".into())
            } else {
                (false, "no device JWT in the access-token slot".into())
            }
        }
        Ok(true) => (
            false,
            "device JWT missing, expired, or near expiry (needs refresh)".into(),
        ),
        Err(e) => (false, format!("could not read device-JWT freshness: {e}")),
    }
}

/// Run ONLY check 5 (device JWT live) against the default `AuthManager` and
/// return its structured [`CheckResult`]. This is the single check Phase 5
/// gates "ready" on: a runner is provisioning-complete only once it holds a
/// live coord device JWT. Reuses [`device_jwt_live_predicate`] — it does NOT
/// reimplement the device-JWT freshness logic.
pub fn device_jwt_live_check() -> CheckResult {
    let auth = crate::auth::AuthManager::new();
    let (ok, detail) = device_jwt_live_predicate(&auth);
    CheckResult {
        name: DEVICE_JWT_LIVE_CHECK_NAME.to_string(),
        ok,
        detail,
        fix: DEVICE_JWT_LIVE_CHECK_FIX.to_string(),
    }
}

// ===========================================================================
// Phase 5 — provisioning completeness gate.
//
// A runner should not report "ready" until it holds a live coord device JWT
// (doctor check 5). The gate is ADVISORY by default — it surfaces the gap
// (via the Phase-1b credential-status path) and logs loudly WITHOUT blocking
// the runner from starting (a hard block could brick a runner mid-provision).
// Setting `QONTINUI_PROVISIONING_GATE_ENFORCE` to a non-`0` value flips it to
// ENFORCING: readiness is withheld until check 5 passes.
// ===========================================================================

/// Env flag that flips the provisioning gate from advisory (default) to
/// enforcing. Unset / `0` / `false` ⇒ advisory.
pub const PROVISIONING_GATE_ENFORCE_ENV: &str = "QONTINUI_PROVISIONING_GATE_ENFORCE";

/// Outcome of the provisioning-completeness gate over doctor check 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvisioningReadiness {
    /// Check 5 passed (a live device JWT) ⇒ provisioning-complete, report ready.
    Ready,
    /// Check 5 failed but the gate is ADVISORY ⇒ surface + log, but DO NOT
    /// withhold ready (never brick a runner mid-provision).
    IncompleteAdvisory,
    /// Check 5 failed AND the gate is ENFORCING ⇒ withhold ready; the runner
    /// is provisioning-incomplete until a device JWT goes live.
    IncompleteEnforced,
}

impl ProvisioningReadiness {
    /// Whether the runner may report "ready" given this verdict. `true` for
    /// `Ready` and for `IncompleteAdvisory` (advisory never blocks); only
    /// `IncompleteEnforced` withholds readiness.
    pub fn ready_to_report(self) -> bool {
        !matches!(self, ProvisioningReadiness::IncompleteEnforced)
    }

    /// Whether the runner is provisioning-INCOMPLETE (check 5 red), regardless
    /// of advisory-vs-enforce. Drives the Phase-1b credential-status surface.
    pub fn is_incomplete(self) -> bool {
        !matches!(self, ProvisioningReadiness::Ready)
    }
}

/// Pure provisioning-gate predicate: given check 5's pass/fail and whether the
/// gate is enforcing, decide the readiness verdict. Pure over its two inputs so
/// it is unit-testable with no live runner / no env. The caller resolves
/// `check5_ok` from [`device_jwt_live_check`] and `enforce` from
/// [`provisioning_gate_enforce_enabled`].
pub fn provisioning_readiness(check5_ok: bool, enforce: bool) -> ProvisioningReadiness {
    match (check5_ok, enforce) {
        (true, _) => ProvisioningReadiness::Ready,
        (false, false) => ProvisioningReadiness::IncompleteAdvisory,
        (false, true) => ProvisioningReadiness::IncompleteEnforced,
    }
}

/// Resolve the advisory-vs-enforce flag from
/// [`PROVISIONING_GATE_ENFORCE_ENV`]. Enforcing only when the var is set to a
/// non-empty value other than `0`/`false` (default: advisory).
pub fn provisioning_gate_enforce_enabled() -> bool {
    match std::env::var(PROVISIONING_GATE_ENFORCE_ENV) {
        Ok(v) => {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Check 1 — Claude account credentials (relocated pure file logic).
//
// Mirrors `ai_provider::oauth_refresh::{find_creds_path, creds_path_is_valid,
// is_expired}` for the UNEXPIRED case (the doctor reports a verdict; it does
// NOT silently refresh, so it deliberately does not invoke the OAuth refresh
// grant — an expired-but-refreshable account reads as red, prompting /login,
// which is the honest doctor answer rather than a side-effecting refresh).
// ---------------------------------------------------------------------------

fn find_creds_path(config_dir: Option<&str>) -> Option<PathBuf> {
    if let Some(dir) = config_dir {
        let p = PathBuf::from(dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = PathBuf::from(&dir).join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".claude").join(".credentials.json");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// True iff the creds file exists and its `claudeAiOauth.expiresAt` (ms) is in
/// the future (or absent — a creds file with no expiry is treated as live,
/// matching the source predicate's `is_expired == false` branch).
fn creds_path_is_valid(creds_path: &Path) -> bool {
    let content = match std::fs::read_to_string(creds_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let expires_at_ms = json["claudeAiOauth"]["expiresAt"].as_i64().unwrap_or(0);
    if expires_at_ms == 0 {
        return true; // no expiry recorded → treat as live
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    expires_at_ms > now_ms
}

fn claude_account_has_valid_credentials(config_dir: Option<&str>) -> bool {
    if let Some(path) = find_creds_path(config_dir) {
        if creds_path_is_valid(&path) {
            return true;
        }
    }
    // macOS stores Claude Code's OAuth credentials in the login Keychain
    // (service "Claude Code-credentials"), NOT in ~/.claude/.credentials.json —
    // so the file-only check above reports a logged-in operator as
    // unauthenticated, telling them to "run /login" when they already have.
    // Accept the Keychain item as proof of a Claude account.
    #[cfg(target_os = "macos")]
    {
        if macos_keychain_has_claude_credentials() {
            return true;
        }
    }
    false
}

/// True iff the macOS login Keychain holds a Claude Code credentials item.
///
/// Queries item ATTRIBUTES only (no `-w`/`-g`), so it never reads the secret
/// and therefore never triggers a Keychain access-control prompt. Presence of
/// the item means the operator has completed `/login` on this machine (Claude
/// Code refreshes the token itself, so on-disk expiry isn't meaningful here).
#[cfg(target_os = "macos")]
fn macos_keychain_has_claude_credentials() -> bool {
    std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Check 2 — runner tier. Minimal read of `settings.json::tier` (the same file
// `settings::load_settings()` reads), so the lib needn't pull the whole
// `Settings` struct. Returns the snake_case tier string.
// ---------------------------------------------------------------------------

fn settings_json_path() -> Option<PathBuf> {
    let dir = std::env::var("QONTINUI_CONFIG_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("com.qontinui.runner")))?;
    Some(dir.join("settings.json"))
}

/// The persisted runner tier as the serde snake_case string
/// (`"local"` | `"local_provider"` | `"qontinui_account"`), or `None` when the
/// settings file is absent/unreadable (which `Settings::default()` would treat
/// as `Local`).
fn read_runner_tier() -> Option<String> {
    let path = settings_json_path()?;
    let bytes = std::fs::read(path).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("tier")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Check 4 — tenant resolution. Mirrors
// `device_jwt_refresher::resolve_pair_tenant_id`'s ordered chain without
// depending on the runner binary: OAuth/runner-bearer claim → outgoing
// device-JWT claim (same slot) → machine.json::active_tenant_id.
//
// Phase 8b semantics: this diagnoses the DEVICE-LEVEL DEFAULT binding
// (machine.json's active_tenant_id is default-for-new-sessions, not
// the-only-tenant). A multi-bound device with per-session tenants can be
// healthy for its default while individual sessions run under other
// bindings — the doctor's check is scoped to the default surface.
// ---------------------------------------------------------------------------

fn read_active_tenant_id_from_machine_json() -> Option<uuid::Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    uuid::Uuid::parse_str(raw.trim()).ok()
}

/// `(tenant, source-label)` from the ordered chain, or `None`. `bearer` is the
/// access-token-slot token, which doubles as both the "OAuth claim" and the
/// "outgoing device-JWT" source here (the doctor has one bearer in hand).
fn resolve_tenant_for_doctor(bearer: &str) -> Option<(uuid::Uuid, &'static str)> {
    if let Some(t) = crate::pair::tenant_id_from_oauth_claim(bearer)
        .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
    {
        return Some((t, "access-token JWT claim"));
    }
    if let Some(t) = read_active_tenant_id_from_machine_json() {
        return Some((t, "machine.json::active_tenant_id"));
    }
    None
}

// ---------------------------------------------------------------------------
// Check 6 — `.mcp.json` validity. Mirrors `coord_mcp::proxy_request_gate`:
// the config's coord-mcp port must equal the live bound port, its nonce must
// be a registered loopback key (persisted in secure storage), and the stored
// bearer must decode `sub_type == "device"`.
// ---------------------------------------------------------------------------

/// Extract `(port, nonce)` from a session `.mcp.json`'s coord-mcp loopback
/// proxy entry, if it is the proxy shape (`url` = `http://127.0.0.1:<port>/
/// coord-mcp`, header `X-Coord-Mcp-Proxy-Key`). `None` for a static-bearer
/// (agent-path) config or a non-coord config.
fn parse_mcp_json_proxy(path: &Path) -> Option<(u16, String)> {
    let bytes = std::fs::read(path).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let server = v.get("mcpServers")?.get("coord-mcp")?;
    let url = server.get("url")?.as_str()?;
    // Expect http://127.0.0.1:<port>/coord-mcp
    let after = url.strip_prefix("http://127.0.0.1:")?;
    let port_str = after.strip_suffix("/coord-mcp")?;
    let port: u16 = port_str.parse().ok()?;
    let nonce = server
        .get("headers")?
        .get("X-Coord-Mcp-Proxy-Key")?
        .as_str()?
        .to_string();
    Some((port, nonce))
}

fn jwt_sub_type(token: &str) -> Option<String> {
    let parts: Vec<&str> = token.trim().split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("sub_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn mcp_json_check(bound_port: Option<u16>, auth: &crate::auth::AuthManager) -> (bool, String) {
    // Look for a coord-mcp proxy `.mcp.json` in the cwd, then the repo root.
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Ok(cwd) = std::env::current_dir() {
            v.push(cwd.join(".mcp.json"));
            // one level up (common: cwd is a sub-repo, config at repo root)
            if let Some(parent) = cwd.parent() {
                v.push(parent.join(".mcp.json"));
            }
        }
        v
    };
    let found = candidates
        .into_iter()
        .find_map(|p| parse_mcp_json_proxy(&p).map(|pr| (p, pr)));
    let Some((path, (cfg_port, nonce))) = found else {
        return (
            false,
            "no coord-mcp proxy .mcp.json found in cwd or repo root".into(),
        );
    };

    // Port == bound port (when we know the bound port).
    match bound_port {
        Some(bp) if bp != cfg_port => {
            return (
                false,
                format!(
                    "{}: coord-mcp port :{cfg_port} != bound port :{bp}",
                    path.display()
                ),
            );
        }
        None => {
            // The standalone bin can't know the live bound port. Validate the
            // nonce + bearer (still meaningful) but flag the port unverified.
            // Treat as red so the operator runs it from inside the runner for
            // the authoritative answer — honest about uncertainty.
            let nonce_ok = nonce_is_registered(&nonce);
            return (
                false,
                format!(
                    "{}: bound port unknown (run from inside the running runner); \
                     nonce {} registered",
                    path.display(),
                    if nonce_ok { "is" } else { "is NOT" }
                ),
            );
        }
        _ => {}
    }

    // Nonce must be a registered loopback key (persisted store).
    if !nonce_is_registered(&nonce) {
        return (
            false,
            format!("{}: nonce is not a registered proxy key", path.display()),
        );
    }

    // Bearer must decode sub_type == device (mirrors proxy_request_gate).
    let bearer = auth.get_access_token().ok().unwrap_or_default();
    match jwt_sub_type(&bearer).as_deref() {
        Some("device") => (
            true,
            format!(".mcp.json port :{cfg_port}, nonce registered, device bearer"),
        ),
        other => (
            false,
            format!("access-token bearer is not a coord DEVICE JWT (sub_type={other:?})"),
        ),
    }
}

/// Whether `nonce` is in the persisted coord-mcp loopback nonce set. Mirrors
/// `coord_mcp::proxy_nonce_is_valid` for the headless/bin path (which has no
/// in-memory `PROXY_NONCES` map — it reads the encrypted store the running
/// runner mirrors to).
fn nonce_is_registered(nonce: &str) -> bool {
    if nonce.is_empty() {
        return false;
    }
    let map: HashMap<String, String> = match crate::secure_storage::SecureStorage::new() {
        Ok(s) => s.load_coord_mcp_nonces(),
        Err(_) => return false,
    };
    map.contains_key(nonce)
}

// ---------------------------------------------------------------------------
// Check 7 — coord reachable. One-shot `tools/list` JSON-RPC against the
// configured coord `/mcp` endpoint, with the device bearer if present.
// ---------------------------------------------------------------------------

fn coord_reachable_check() -> (bool, String) {
    let base = match crate::profiles::resolve_coord_base() {
        crate::profiles::CoordBase::Configured(b) | crate::profiles::CoordBase::DevLocalhost(b) => {
            b
        }
        crate::profiles::CoordBase::Unset => "http://localhost:9870".to_string(),
    };
    let url = format!("{}/mcp", base.trim_end_matches('/'));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });
    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("could not build HTTP client: {e}")),
    };
    let mut rb = client.post(&url).json(&body);
    if let Some(bearer) = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        rb = rb.header("Authorization", format!("Bearer {bearer}"));
    }
    match rb.send() {
        Ok(resp) if resp.status().is_success() => {
            (true, format!("coord /mcp tools/list returned 200 ({url})"))
        }
        Ok(resp) => (
            false,
            format!("coord /mcp returned HTTP {} ({url})", resp.status()),
        ),
        Err(e) => (false, format!("coord /mcp unreachable ({url}): {e}")),
    }
}

// ===========================================================================
// Tests — the pure driver (ordering / first-red-stops) is fully exercised
// here without any live runner.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn green(name: &'static str) -> Check<'static> {
        Check::new(name, "fix-it", || (true, "green".into()))
    }
    fn red(name: &'static str) -> Check<'static> {
        Check::new(name, "fix-it", || (false, "red".into()))
    }

    #[test]
    fn all_green_runs_every_check_and_passes() {
        let report = run_checks(vec![green("a"), green("b"), green("c")]);
        assert!(report.overall_ok);
        assert_eq!(report.checks.len(), 3);
        assert!(report.checks.iter().all(|c| c.ok));
    }

    #[test]
    fn stops_at_first_red_and_omits_later_checks() {
        let report = run_checks(vec![green("a"), red("b"), green("c")]);
        assert!(!report.overall_ok);
        // Only a (green) + b (red) ran; c never executed.
        assert_eq!(report.checks.len(), 2);
        assert_eq!(report.checks[0].name, "a");
        assert!(report.checks[0].ok);
        assert_eq!(report.checks[1].name, "b");
        assert!(!report.checks[1].ok);
    }

    #[test]
    fn first_check_red_short_circuits_immediately() {
        let report = run_checks(vec![red("a"), green("b")]);
        assert!(!report.overall_ok);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "a");
    }

    #[test]
    fn checks_run_in_declared_order() {
        // A side-effect log proves the driver preserves order and stops.
        use std::cell::RefCell;
        use std::rc::Rc;
        let log: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
        let mk = |name: &'static str, ok: bool| {
            let log = log.clone();
            Check::new(name, "fix", move || {
                log.borrow_mut().push(name);
                (ok, "d".into())
            })
        };
        let _ = run_checks(vec![
            mk("one", true),
            mk("two", true),
            mk("three", false),
            mk("four", true),
        ]);
        assert_eq!(*log.borrow(), vec!["one", "two", "three"]);
    }

    #[test]
    fn render_marks_pass_fail_and_names_the_block() {
        let report = run_checks(vec![green("alpha"), red("beta"), green("gamma")]);
        let text = report.render();
        assert!(text.contains("[PASS] alpha"));
        assert!(text.contains("[FAIL] beta"));
        // gamma never ran → not in the report.
        assert!(!text.contains("gamma"));
        assert!(text.contains("BLOCKED at: beta"));
        assert!(text.contains("fix: fix-it"));
    }

    #[test]
    fn render_all_green_says_can_set_gates() {
        let report = run_checks(vec![green("a"), green("b")]);
        let text = report.render();
        assert!(text.contains("OK: this runner can set gates."));
    }

    #[test]
    fn empty_check_set_is_vacuously_ok() {
        let report = run_checks(vec![]);
        assert!(report.overall_ok);
        assert!(report.checks.is_empty());
    }

    // ---- pure helpers ----

    #[test]
    fn parse_mcp_json_proxy_reads_port_and_nonce() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"http://127.0.0.1:9877/coord-mcp",
               "headers":{"X-Coord-Mcp-Proxy-Key":"abc123"}}}}"#,
        )
        .unwrap();
        let (port, nonce) = parse_mcp_json_proxy(&path).expect("parses proxy config");
        assert_eq!(port, 9877);
        assert_eq!(nonce, "abc123");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn parse_mcp_json_proxy_none_for_static_bearer_config() {
        let dir = std::env::temp_dir().join("qontinui_doctor_test_mcp2");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".mcp.json");
        // Agent/static-bearer shape has no loopback url → not a proxy config.
        std::fs::write(
            &path,
            r#"{"mcpServers":{"coord-mcp":{"type":"http",
               "url":"https://coord.qontinui.io/mcp",
               "headers":{"Authorization":"Bearer xyz"}}}}"#,
        )
        .unwrap();
        assert!(parse_mcp_json_proxy(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn jwt_sub_type_decodes_device() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"sub":"x","sub_type":"device"}"#);
        let token = format!("{header}.{payload}.sig");
        assert_eq!(jwt_sub_type(&token).as_deref(), Some("device"));
        // An agent JWT decodes to agent, an opaque token to None.
        let agent_payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"sub_type":"agent"}"#);
        let agent = format!("{header}.{agent_payload}.sig");
        assert_eq!(jwt_sub_type(&agent).as_deref(), Some("agent"));
        assert_eq!(jwt_sub_type("opaque-token"), None);
    }

    // ---- Phase 5 — provisioning completeness gate (pure predicate) ----

    #[test]
    fn provisioning_ready_when_check5_passes_regardless_of_enforce() {
        // A live device JWT ⇒ Ready, advisory or enforcing.
        assert_eq!(
            provisioning_readiness(true, false),
            ProvisioningReadiness::Ready
        );
        assert_eq!(
            provisioning_readiness(true, true),
            ProvisioningReadiness::Ready
        );
        assert!(provisioning_readiness(true, true).ready_to_report());
        assert!(!provisioning_readiness(true, true).is_incomplete());
    }

    #[test]
    fn provisioning_incomplete_advisory_does_not_withhold_ready() {
        // Check 5 red + advisory ⇒ incomplete, but STILL allowed to report
        // ready (never brick a runner mid-provision).
        let v = provisioning_readiness(false, false);
        assert_eq!(v, ProvisioningReadiness::IncompleteAdvisory);
        assert!(v.is_incomplete(), "must surface as provisioning-incomplete");
        assert!(
            v.ready_to_report(),
            "advisory must NOT withhold ready (no hard block)"
        );
    }

    #[test]
    fn provisioning_incomplete_enforced_withholds_ready() {
        // Check 5 red + enforcing ⇒ incomplete AND ready withheld.
        let v = provisioning_readiness(false, true);
        assert_eq!(v, ProvisioningReadiness::IncompleteEnforced);
        assert!(v.is_incomplete());
        assert!(
            !v.ready_to_report(),
            "enforcing must withhold ready until a device JWT is live"
        );
    }

    #[test]
    fn provisioning_gate_enforce_env_parsing() {
        // Save/restore the process-global so this test is self-contained. This
        // is the ONLY test that touches the flag; no sibling reads it, so the
        // save/restore is sufficient (and there's no parallel reader to race).
        let prev = std::env::var(PROVISIONING_GATE_ENFORCE_ENV).ok();

        std::env::remove_var(PROVISIONING_GATE_ENFORCE_ENV);
        assert!(
            !provisioning_gate_enforce_enabled(),
            "unset ⇒ advisory (default)"
        );

        for off in ["0", "", "false", "False", "  "] {
            std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, off);
            assert!(
                !provisioning_gate_enforce_enabled(),
                "{off:?} must read as advisory"
            );
        }
        for on in ["1", "true", "yes", "enforce"] {
            std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, on);
            assert!(
                provisioning_gate_enforce_enabled(),
                "{on:?} must read as enforcing"
            );
        }

        match prev {
            Some(p) => std::env::set_var(PROVISIONING_GATE_ENFORCE_ENV, p),
            None => std::env::remove_var(PROVISIONING_GATE_ENFORCE_ENV),
        }
    }
}
