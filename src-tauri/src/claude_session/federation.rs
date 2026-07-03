//! Spawn-site glue between [`ClaudeSession`] / `run_claude_session_inline`
//! and [`observable_bridge`](qontinui_runner_lib::observable_bridge).
//!
//! Plan: `2026-05-22-memories-on-coord-cross-machine.md` Phase 5.
//!
//! Two responsibilities only:
//!
//! 1. **[`build_federation_ctx`]** — assemble the [`SessionContext`] every
//!    spawn site needs (tenant_id + device_id + account_name + memory_dir
//!    + session_id). Returns `None` when any required identity field is
//!    missing; spawn sites then skip federation and proceed locally.
//! 2. **[`emit_federation_report`]** — uniform telemetry surface for
//!    `reconcile`'s `ReconcileReport`. Logs via `tracing` and broadcasts
//!    on the Tauri event channel `memory-federation:reconcile` so the
//!    React frontend can render a banner.
//!
//! No business logic lives here — the bridge itself owns pull / push /
//! reconcile. This module is the spawn-site adapter only.
//!
//! [`ClaudeSession`]: crate::claude_session::ClaudeSession
//! [`SessionContext`]: qontinui_runner_lib::observable_bridge::SessionContext

use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;
use tauri::Emitter;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::settings::ClaudeCliSettings;
use qontinui_runner_lib::observable_bridge::{ReconcileReport, SessionContext};

/// Tauri event channel for reconcile-result broadcasts. The React
/// frontend listens here to render the "Memory federation: N pushed,
/// M pulled, K failed" banner described in plan Phase 5.
pub const RECONCILE_EVENT: &str = "memory-federation:reconcile";

/// Tauri event channel for federation-skipped broadcasts. Mirrors
/// [`RECONCILE_EVENT`]: when federation is skipped for a session the
/// spawn site emits a one-line `{ session_id, reason, detail }` payload
/// here so the React frontend can surface a "federation: skipped
/// (<reason>)" notice instead of the old silent warn-only behavior.
pub const SKIP_EVENT: &str = "memory-federation:skipped";

/// Tauri event channel for reconcile-LEVEL failure broadcasts. Distinct
/// from per-file `failed_names` (which ride on [`RECONCILE_EVENT`]): this
/// fires when the whole reconcile returns `Err` and no `ReconcileReport`
/// is produced at all. Mirrors [`SKIP_EVENT`]: the spawn site emits a
/// one-line `{ session_id, account, error }` payload here so the React
/// frontend can surface a sticky "reconcile failed" notice instead of the
/// old silent warn-only behavior.
pub const ERROR_EVENT: &str = "memory-federation:error";

/// Why memory federation was skipped for a session. Returned by
/// [`build_federation_ctx`] (as the `Err` arm) so the spawn site can
/// surface the reason to the UI via [`SKIP_EVENT`] instead of silently
/// dropping a bare `None`. The `disabled` (kill-switch) case is not part
/// of this enum because it short-circuits before `build_federation_ctx`
/// is ever called — the call site emits `reason = "disabled"` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FederationSkipReason {
    /// No `paired_user.json::tenant_id` and no claim in the cached
    /// device-token JWT — the runner isn't paired to a tenant.
    TenantUnresolved,
    /// `~/.qontinui/machine.json` is missing or unparseable.
    DeviceIdMissing,
    /// No effective Claude config dir could be resolved.
    NoConfigDir,
}

impl FederationSkipReason {
    /// Stable machine-readable reason token, mirrored by the frontend
    /// skip-notice copy. Kept in sync with `MemoryFederationBanner.tsx`.
    pub fn as_str(&self) -> &'static str {
        match self {
            FederationSkipReason::TenantUnresolved => "tenant-unresolved",
            FederationSkipReason::DeviceIdMissing => "device-id-missing",
            FederationSkipReason::NoConfigDir => "no-config-dir",
        }
    }

    /// Short human-readable explanation for the UI skip notice. Kept
    /// concise (one short clause) since the frontend renders it inline.
    pub fn detail(&self) -> &'static str {
        match self {
            FederationSkipReason::TenantUnresolved => "Runner not paired to a tenant.",
            FederationSkipReason::DeviceIdMissing => {
                "Device identity (~/.qontinui/machine.json) is missing."
            }
            FederationSkipReason::NoConfigDir => "No effective Claude config directory.",
        }
    }
}

impl std::fmt::Display for FederationSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Tauri event payload for [`SKIP_EVENT`]. `reason` is a stable token
/// (see [`FederationSkipReason::as_str`] plus the `"disabled"`
/// kill-switch case); `detail` is a short human-readable explanation.
#[derive(Debug, Clone, Serialize)]
pub struct SkipPayload {
    pub session_id: String,
    pub reason: String,
    pub detail: String,
}

/// Emit a [`SKIP_EVENT`] for `session_id` with the given reason token and
/// detail. Best-effort: an emit failure is logged at `debug` and never
/// propagated, mirroring [`emit_federation_report`]'s emit handling. The
/// caller is responsible for the accompanying `warn!` (preserved at each
/// skip site).
pub fn emit_federation_skip(
    app_handle: &tauri::AppHandle,
    session_id: &str,
    reason: &str,
    detail: &str,
) {
    let payload = SkipPayload {
        session_id: session_id.to_string(),
        reason: reason.to_string(),
        detail: detail.to_string(),
    };
    if let Err(e) = app_handle.emit(SKIP_EVENT, &payload) {
        debug!("claude_session::federation: emit {SKIP_EVENT} failed: {e} (non-fatal)");
    }
}

/// Tauri event payload for [`ERROR_EVENT`]. `error` is the
/// `Display`-formatted reconcile-level error string.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorPayload {
    pub session_id: String,
    pub account: String,
    pub error: String,
}

/// Emit an [`ERROR_EVENT`] for `session_id` carrying the reconcile-level
/// failure. Best-effort: an emit failure is logged at `debug` and never
/// propagated, mirroring [`emit_federation_skip`]'s emit handling. The
/// caller is responsible for the accompanying `warn!` (preserved at the
/// failure site).
pub fn emit_federation_error(
    app_handle: &tauri::AppHandle,
    session_id: &str,
    account: &str,
    error: &str,
) {
    let payload = ErrorPayload {
        session_id: session_id.to_string(),
        account: account.to_string(),
        error: error.to_string(),
    };
    if let Err(e) = app_handle.emit(ERROR_EVENT, &payload) {
        debug!("claude_session::federation: emit {ERROR_EVENT} failed: {e} (non-fatal)");
    }
}

/// Cross-account memory federation kill switch — reads from
/// `AiSettings::memory_federation_enabled`. Default ON; the field is
/// `#[serde(default = "default_memory_federation_enabled")]` so the
/// missing-on-existing-settings case still resolves to true.
pub fn federation_enabled() -> bool {
    crate::settings::get_ai_settings().memory_federation_enabled
}

/// Convert a `&str` session id into a UUID. The runner uses several
/// session-id schemes (raw UUIDs for workflows, `task_run_id + phase`
/// strings for interactive sessions). The bridge keys watchers by UUID
/// only, so non-UUID ids get a deterministic v5-style hash so two
/// reconciles for the same string still collide on the same watcher
/// slot.
pub fn session_uuid(session_id: &str) -> Uuid {
    if let Ok(u) = Uuid::parse_str(session_id) {
        return u;
    }
    // UUID v5 of the runner's federation namespace + session_id. Stable
    // across the process lifetime so `stop_watcher(session_id_of(s))`
    // matches the `start_watcher` call site for the same string.
    static NS: Uuid = Uuid::from_bytes([
        0x6f, 0x6e, 0x74, 0x69, 0x6e, 0x75, 0x69, 0x2d, 0x66, 0x65, 0x64, 0x65, 0x72, 0x61, 0x74,
        0x65,
    ]);
    Uuid::new_v5(&NS, session_id.as_bytes())
}

/// Build a `SessionContext` for the about-to-spawn Claude subprocess.
/// Returns `Err(FederationSkipReason)` (with a warn log) when any required
/// identity field is missing — that signals the spawn site to skip
/// federation (and surface the reason to the UI via [`SKIP_EVENT`]).
///
/// Resolution sources:
///
/// - `tenant_id` → **the spawned session's own binding first** (Phase 8b,
///   plan 2026-07-02-session-scoped-multi-tenant-device-binding §D4:
///   `session_tenant`, threaded from the spawn input when the caller
///   recorded one), else the device DEFAULT binding:
///   `paired_user.json::default_tenant_id`, falling back to the cached
///   device-token JWT claim. Memories are hard tenant-scoped, so a
///   session spawned for tenant B must federate B's pool even when the
///   machine default is A.
/// - `device_id` → `~/.qontinui/machine.json::device_id`.
/// - `account_name` → derived from `effective_config_dir` (trailing
///   `.claude-<name>` component). Falls back to `"default"` for a
///   single-account install where `config_dir` is `~/.claude`.
/// - `memory_dir` → `<effective_config_dir>/projects/<encoded
///   working_dir>/memory/` per Claude Code's on-disk convention.
///
/// The home-dir identity PIN (`federation.rs` device identity) is
/// deliberately untouched (plan §6).
pub fn build_federation_ctx(
    session_id: &str,
    working_dir: &str,
    cli_settings: &ClaudeCliSettings,
    session_tenant: Option<Uuid>,
) -> Result<SessionContext, FederationSkipReason> {
    let tenant_id = match session_tenant.or_else(resolve_tenant_id) {
        Some(t) => t,
        None => {
            warn!(
                "claude_session::federation: tenant_id unresolvable \
                 (no session tenant, no paired_user.json::tenant_id, no claim in \
                 cached device-token JWT); \
                 skipping memory federation for session {session_id}"
            );
            return Err(FederationSkipReason::TenantUnresolved);
        }
    };

    let device_id = match resolve_device_id() {
        Some(d) => d,
        None => {
            warn!(
                "claude_session::federation: ~/.qontinui/machine.json missing or unparseable; \
                 skipping memory federation for session {session_id}"
            );
            return Err(FederationSkipReason::DeviceIdMissing);
        }
    };

    let effective_config_dir = crate::ai_provider::get_effective_config_dir(cli_settings);
    let config_dir_str = match effective_config_dir {
        Some(s) => s,
        None => {
            warn!(
                "claude_session::federation: no effective claude config_dir; \
                 skipping memory federation for session {session_id}"
            );
            return Err(FederationSkipReason::NoConfigDir);
        }
    };

    let memory_dir = derive_memory_dir(&config_dir_str, working_dir);
    let account_name = derive_account_name(&config_dir_str);

    Ok(SessionContext {
        tenant_id,
        device_id,
        account_name,
        memory_dir,
        working_dir: PathBuf::from(working_dir),
        session_id: session_uuid(session_id),
        post_pull_snapshot: None,
        pulled_count: 0,
    })
}

/// Tauri event payload mirroring [`ReconcileReport`] plus session
/// identity. Serialized straight onto `RECONCILE_EVENT`.
#[derive(Debug, Clone, Serialize)]
struct ReconcilePayload<'a> {
    session_id: String,
    account: &'a str,
    report: ReconcileReport,
}

/// Surface a `reconcile` result on stderr (`tracing`) + the Tauri event
/// channel + best-effort POST to coord. Emit failures are swallowed
/// because telemetry must never block the session-end cleanup path.
pub fn emit_federation_report(
    app_handle: &tauri::AppHandle,
    ctx: &SessionContext,
    report: Result<ReconcileReport>,
) {
    match report {
        Ok(r) => {
            info!(
                session_id = %ctx.session_id,
                account = %ctx.account_name,
                pushed = r.pushed,
                pulled = r.pulled,
                unchanged = r.unchanged,
                failed = r.failed,
                "memory federation reconcile complete"
            );

            // Best-effort POST to coord — fire-and-forget so we never
            // block the session-end cleanup path.
            post_report_to_coord(ctx, &r);

            let payload = ReconcilePayload {
                session_id: ctx.session_id.to_string(),
                account: &ctx.account_name,
                report: r,
            };
            if let Err(e) = app_handle.emit(RECONCILE_EVENT, &payload) {
                debug!(
                    "claude_session::federation: emit {RECONCILE_EVENT} failed: {e} (non-fatal)"
                );
            }
        }
        Err(e) => {
            warn!(
                session_id = %ctx.session_id,
                account = %ctx.account_name,
                error = %e,
                "memory federation reconcile failed"
            );
            // Surface the reconcile-LEVEL failure on the dashboard. Unlike
            // per-file `failed_names` (which ride on RECONCILE_EVENT), a
            // whole-reconcile `Err` produces no report at all, so without
            // this the dashboard would show nothing.
            emit_federation_error(
                app_handle,
                &ctx.session_id.to_string(),
                &ctx.account_name,
                &e.to_string(),
            );
        }
    }
}

/// Best-effort POST of the reconcile report to coord's federation reports
/// endpoint. Runs detached so the caller is never blocked. Errors are logged
/// but never propagated.
///
/// Runtime-aware: `log_federation_report` is documented as reachable from
/// "contexts that have a `tracing` logger but no Tauri AppHandle" — and in
/// practice the memory-reconcile path runs on a `spawn_blocking` worker / plain
/// `std::thread` with NO ambient Tokio runtime. A bare `tokio::spawn` there
/// panics with "there is no reactor running, must be called from the context of
/// a Tokio 1.x runtime", killing the reconcile thread. So: use the ambient
/// runtime when one exists (the fast path), else run the post on a throwaway
/// current-thread runtime in a detached `std::thread` — never panic, never
/// block the caller.
fn post_report_to_coord(ctx: &SessionContext, report: &ReconcileReport) {
    use qontinui_runner_lib::observable_bridge::memory_client;

    let token = match crate::auth::AuthManager::new().get_access_token() {
        Ok(t) if !t.is_empty() => t,
        _ => return, // not paired
    };
    let base = match memory_client::coord_http_base() {
        Some(b) => b,
        None => return, // no coord URL
    };

    let failed_names: Option<serde_json::Value> = if report.failed_names.is_empty() {
        None
    } else {
        Some(serde_json::json!(report.failed_names))
    };

    let body = serde_json::json!({
        "device_id": ctx.device_id,
        "session_id": ctx.session_id,
        "account_name": ctx.account_name,
        "pushed": report.pushed,
        "pulled": report.pulled,
        "unchanged": report.unchanged,
        "failed": report.failed,
        "failed_names": failed_names,
        "metadata": {
            "coord_url": base,
        },
    });

    let http = match memory_client::build_client() {
        Ok(c) => c,
        Err(e) => {
            debug!("federation report: build_client failed: {e} (non-fatal)");
            return;
        }
    };

    spawn_detached_best_effort(async move {
        if let Err(e) = memory_client::post_federation_report(&http, &base, &token, &body).await {
            debug!("federation report: coord POST failed: {e} (non-fatal)");
        }
    });
}

/// Spawn `fut` detached without ever panicking on the caller's runtime context.
///
/// `tokio::spawn` panics ("there is no reactor running") when invoked outside a
/// Tokio runtime — which the federation-report callers can be (they run on a
/// `spawn_blocking` worker / plain `std::thread`). This uses the ambient
/// runtime when one exists (the fast path), else runs `fut` to completion on a
/// throwaway current-thread runtime in a detached `std::thread`. Either way the
/// caller is never blocked. Factored out (and `pub(crate)`) so the no-panic
/// invariant is unit-testable without a real reconcile.
pub(crate) fn spawn_detached_best_effort<F>(fut: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(fut);
        }
        Err(_) => {
            std::thread::spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt.block_on(fut),
                    Err(e) => {
                        debug!("federation report: fallback runtime build failed: {e} (non-fatal)")
                    }
                }
            });
        }
    }
}

/// Reachable from contexts that have a `tracing` logger but no Tauri
/// `AppHandle` (the inline path uses `tracing` only — see plan Phase 5.C
/// "If the inline path doesn't have an `AppHandle` accessible, emit via
/// `tracing::info!`"). Same content as [`emit_federation_report`]
/// minus the Tauri broadcast.
pub fn log_federation_report(ctx: &SessionContext, report: Result<ReconcileReport>) {
    match report {
        Ok(r) => {
            info!(
                session_id = %ctx.session_id,
                account = %ctx.account_name,
                pushed = r.pushed,
                pulled = r.pulled,
                unchanged = r.unchanged,
                failed = r.failed,
                "memory federation reconcile complete"
            );
            post_report_to_coord(ctx, &r);
        }
        Err(e) => warn!(
            session_id = %ctx.session_id,
            account = %ctx.account_name,
            error = %e,
            "memory federation reconcile failed"
        ),
    }
}

/// Generic per-bridge reconcile telemetry for non-memory observable
/// categories. Logs the aggregate `ReconcileReport` via `tracing` tagged
/// with the bridge `category`. Unlike [`emit_federation_report`] it does
/// NOT POST to coord or broadcast the memory-specific Tauri banner event —
/// each non-memory bridge owns its own coord telemetry inside its
/// `reconcile`/client. Keeps the session-end path crash-free for any
/// number of registered bridges.
pub fn log_bridge_report(category: &str, ctx: &SessionContext, report: Result<ReconcileReport>) {
    match report {
        Ok(r) => info!(
            category = %category,
            session_id = %ctx.session_id,
            account = %ctx.account_name,
            pushed = r.pushed,
            pulled = r.pulled,
            unchanged = r.unchanged,
            failed = r.failed,
            "federation reconcile complete"
        ),
        Err(e) => warn!(
            category = %category,
            session_id = %ctx.session_id,
            account = %ctx.account_name,
            error = %e,
            "federation reconcile failed"
        ),
    }
}

/// Resolve the device DEFAULT binding's tenant UUID (Phase 8b semantics —
/// the fallback for sessions with no recorded tenant of their own).
/// Mirrors `fleet.rs::resolve_tenant_id` but that's private to `fleet`;
/// the duplication here keeps the federation module self-contained.
fn resolve_tenant_id() -> Option<Uuid> {
    if let Some(s) = qontinui_runner_lib::pair::read_paired_tenant_id_from_disk() {
        if let Ok(t) = Uuid::parse_str(s.trim()) {
            return Some(t);
        }
    }
    let token = crate::auth::AuthManager::new()
        .get_access_token()
        .ok()
        .unwrap_or_default();
    if token.is_empty() {
        return None;
    }
    let claim = qontinui_runner_lib::pair::tenant_id_from_oauth_claim(&token)?;
    Uuid::parse_str(claim.trim()).ok()
}

/// Read `~/.qontinui/machine.json::device_id` (with `machine_id` alias
/// for legacy installs). Returns `None` if the file is missing,
/// unparseable, or holds a non-UUID id.
fn resolve_device_id() -> Option<Uuid> {
    #[derive(serde::Deserialize)]
    struct DeviceFile {
        #[serde(alias = "machine_id")]
        device_id: String,
    }
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(&path).ok()?;
    let parsed: DeviceFile = serde_json::from_slice(&bytes).ok()?;
    Uuid::parse_str(parsed.device_id.trim()).ok()
}

/// Derive the per-project memory dir Claude Code reads/writes for the
/// session. Layout (per plan D2):
///
///   `<config_dir>/projects/<encoded(working_dir)>/memory/`
///
/// `encoded` follows the same convention as
/// `terminal::transcript::encode_project_path` — normalize `\` → `/`,
/// then `:/` → `--`, then every `:` `/` `\` `_` `.` → `-`.
fn derive_memory_dir(config_dir: &str, working_dir: &str) -> PathBuf {
    let encoded = encode_workspace_path(working_dir);
    PathBuf::from(config_dir)
        .join("projects")
        .join(encoded)
        .join("memory")
}

/// Workspace-path encoder shared with Claude Code's on-disk projects
/// layout. Returns e.g. `D--qontinui-root` for `D:\qontinui-root`.
///
/// Encoding matches `terminal::transcript::encode_project_path`'s rules
/// (normalize `\` → `/`, then `:/` → `--`, then `: / \ _` → `-`). The
/// transcript encoder is the canonical reference for what lands on disk
/// under `<config_dir>/projects/`.
fn encode_workspace_path(working_dir: &str) -> String {
    let normalized = working_dir
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string();
    normalized
        .replace(":/", "--")
        .replace([':', '/', '\\', '_'], "-")
}

/// Pull a human-readable account label off the `effective_config_dir`.
/// E.g. `C:\claude\.claude-hotmail` → `"hotmail"`; `~/.claude` →
/// `"default"`. The label is purely for telemetry — the bridge keys on
/// `device_id` + `tenant_id`, not account name.
fn derive_account_name(config_dir: &str) -> String {
    // Cross-platform path parsing: `std::path::Path::new` on Linux treats
    // `\` as part of the filename, so a Windows-style path like
    // `"C:\\claude\\.claude-hotmail"` resolves to one big basename on
    // Linux CI runners. Split on both separators ourselves to get the
    // last segment regardless of host OS.
    let basename = config_dir.rsplit(['/', '\\']).next().unwrap_or(config_dir);
    let stripped = basename
        .trim_start_matches('.')
        .strip_prefix("claude-")
        .unwrap_or("");
    if stripped.is_empty() {
        "default".to_string()
    } else {
        stripped.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_drive_letter_path() {
        assert_eq!(
            encode_workspace_path("D:\\qontinui-root"),
            "D--qontinui-root"
        );
        assert_eq!(
            encode_workspace_path("D:/qontinui-root"),
            "D--qontinui-root"
        );
    }

    #[test]
    fn encodes_unix_path() {
        assert_eq!(
            encode_workspace_path("/home/user/work_dir"),
            "-home-user-work-dir"
        );
    }

    #[test]
    fn encodes_path_with_period_preserved() {
        // Dots in path segments are NOT replaced — this matches Claude
        // Code's transcript.rs encoder. If Claude ever changes behavior
        // and starts encoding dots, both encoders must update in
        // lockstep; until then, dots stay.
        assert_eq!(
            encode_workspace_path("/repo/app.config"),
            "-repo-app.config"
        );
    }

    #[test]
    fn derives_account_name_from_hotmail_dir() {
        assert_eq!(
            derive_account_name("C:\\claude\\.claude-hotmail"),
            "hotmail"
        );
    }

    #[test]
    fn derives_default_when_no_account_suffix() {
        assert_eq!(derive_account_name("/home/user/.claude"), "default");
        assert_eq!(derive_account_name("/home/user/.claude-"), "default");
    }

    #[test]
    fn session_uuid_is_deterministic_for_non_uuid_string() {
        let a = session_uuid("task-run-42:setup");
        let b = session_uuid("task-run-42:setup");
        assert_eq!(a, b, "same input must produce same UUID");
        let c = session_uuid("task-run-42:verification");
        assert_ne!(a, c, "different inputs must produce different UUIDs");
    }

    #[test]
    fn session_uuid_passes_through_real_uuid() {
        let raw = "550e8400-e29b-41d4-a716-446655440000";
        let u = session_uuid(raw);
        assert_eq!(u.to_string(), raw);
    }

    #[test]
    fn spawn_detached_runs_future_without_ambient_runtime() {
        // REGRESSION: `post_report_to_coord` previously called `tokio::spawn`
        // directly. The federation-report callers can run with NO ambient
        // Tokio runtime (spawn_blocking worker / std::thread), where
        // `tokio::spawn` panics "there is no reactor running" and kills the
        // reconcile thread. This is a plain `#[test]` (NOT `#[tokio::test]`) so
        // there is deliberately no ambient runtime — the helper MUST take its
        // std::thread + throwaway-runtime fallback path, run the future to
        // completion, and never panic.
        assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "test must run with no ambient Tokio runtime to exercise the fallback"
        );
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        spawn_detached_best_effort(async move {
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("future must run to completion via the fallback runtime");
    }
}
