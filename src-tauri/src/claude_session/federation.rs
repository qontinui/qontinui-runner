//! Spawn-site glue between [`ClaudeSession`] / `run_claude_session_inline`
//! and [`observable_bridge`](qontinui_runner_lib::observable_bridge).
//!
//! Plan: `2026-05-22-memories-on-coord-cross-machine.md` Phase 5,
//! generalized by `2026-05-24-federation-verify-and-gitop.md`. The
//! memory category itself was retired by
//! `2026-07-26-claude-session-memory-cutover-to-coord` Phase 3a; the
//! glue now serves the remaining observable-bridge categories
//! (`git_op` today).
//!
//! Two responsibilities only:
//!
//! 1. **[`build_federation_ctx`]** — assemble the [`SessionContext`] every
//!    spawn site needs (tenant_id + device_id + account_name +
//!    working_dir + session_id). Returns `Err(FederationSkipReason)` when
//!    any required identity field is missing; spawn sites then skip
//!    federation and proceed locally.
//! 2. **[`log_bridge_report`]** — uniform `tracing` telemetry for each
//!    bridge's session-end `ReconcileReport`.
//!
//! No business logic lives here — the bridge itself owns pull / push /
//! reconcile. This module is the spawn-site adapter only.
//!
//! [`ClaudeSession`]: crate::claude_session::ClaudeSession
//! [`SessionContext`]: qontinui_runner_lib::observable_bridge::SessionContext

use std::path::PathBuf;

use anyhow::Result;
use tracing::{info, warn};
use uuid::Uuid;

use crate::settings::ClaudeCliSettings;
use qontinui_runner_lib::observable_bridge::{ReconcileReport, SessionContext};

/// Why federation was skipped for a session. Returned by
/// [`build_federation_ctx`] (as the `Err` arm) so the spawn site can log
/// the reason instead of dropping a bare `None`.
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
    /// Stable machine-readable reason token.
    pub fn as_str(&self) -> &'static str {
        match self {
            FederationSkipReason::TenantUnresolved => "tenant-unresolved",
            FederationSkipReason::DeviceIdMissing => "device-id-missing",
            FederationSkipReason::NoConfigDir => "no-config-dir",
        }
    }
}

impl std::fmt::Display for FederationSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
/// federation.
///
/// Resolution sources:
///
/// - `tenant_id` → **the spawned session's own binding first** (Phase 8b,
///   plan 2026-07-02-session-scoped-multi-tenant-device-binding §D4:
///   `session_tenant`, threaded from the spawn input when the caller
///   recorded one), else the device DEFAULT binding:
///   `paired_user.json::default_tenant_id`, falling back to the cached
///   device-token JWT claim. Federated observables are hard
///   tenant-scoped, so a session spawned for tenant B must federate B's
///   pool even when the machine default is A.
/// - `device_id` → `~/.qontinui/machine.json::device_id`.
/// - `account_name` → derived from `effective_config_dir` (trailing
///   `.claude-<name>` component). Falls back to `"default"` for a
///   single-account install where `config_dir` is `~/.claude`.
///
/// The home-dir identity PIN (`federation.rs` device identity) is
/// deliberately untouched (plan §6).
pub fn build_federation_ctx(
    session_id: &str,
    working_dir: &str,
    cli_settings: &ClaudeCliSettings,
    session_tenant: Option<Uuid>,
) -> Result<SessionContext, FederationSkipReason> {
    let tenant_id = match resolve_session_context_tenant(session_tenant) {
        Some(t) => t,
        None => {
            warn!(
                "claude_session::federation: tenant_id unresolvable \
                 (no session tenant, no paired_user.json::tenant_id, no claim in \
                 cached device-token JWT); \
                 skipping federation for session {session_id}"
            );
            return Err(FederationSkipReason::TenantUnresolved);
        }
    };

    let device_id = match resolve_device_id() {
        Some(d) => d,
        None => {
            warn!(
                "claude_session::federation: ~/.qontinui/machine.json missing or unparseable; \
                 skipping federation for session {session_id}"
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
                 skipping federation for session {session_id}"
            );
            return Err(FederationSkipReason::NoConfigDir);
        }
    };

    let account_name = derive_account_name(&config_dir_str);

    Ok(SessionContext {
        tenant_id,
        device_id,
        account_name,
        working_dir: PathBuf::from(working_dir),
        session_id: session_uuid(session_id),
    })
}

/// Per-bridge reconcile telemetry. Logs the aggregate `ReconcileReport`
/// via `tracing` tagged with the bridge `category`. Each bridge owns its
/// own coord telemetry inside its `reconcile`/client — this keeps the
/// session-end path crash-free for any number of registered bridges.
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

/// The tenant a spawned session's federation binds to — the value
/// that populates [`SessionContext::tenant_id`] (B2). The session's
/// RECORDED tenant wins (`session_tenant`, threaded by the spawn site from
/// `machine.json::active_tenant_id` — spawn input else the active pin —
/// the SAME source `session::mod::stamp_session_tenant` uses for the coord
/// record), so the coord record + JWT slot + federated pool agree by
/// construction. Only when the session recorded no tenant (single-tenant
/// install, no active pin) does this fall back to the device DEFAULT
/// binding via [`resolve_tenant_id`]. Split out so the B2 precedence is
/// hermetically testable without the file/home-dir reads the rest of
/// identity resolution needs.
fn resolve_session_context_tenant(session_tenant: Option<Uuid>) -> Option<Uuid> {
    session_tenant.or_else(resolve_tenant_id)
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

/// Pull a human-readable account label off the `effective_config_dir`.
/// E.g. `C:\claude\.claude-hotmail` → `"hotmail"`; `~/.claude` →
/// `"default"`.
///
/// Used both for telemetry (the bridge keys on `device_id` + `tenant_id`,
/// not account name) and as a **selection key**: per-request account
/// selection (`ai_provider::account_usage::resolve_requested_account`)
/// matches a caller-supplied friendly name against `derive_account_name`
/// of each roster config dir, so this is the single source of truth for
/// the name derivation — do not duplicate the path-parsing elsewhere.
pub(crate) fn derive_account_name(config_dir: &str) -> String {
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
    fn federation_binds_recorded_session_tenant_over_device_default() {
        // B2 REGRESSION: a session's federation `SessionContext.tenant_id`
        // is populated by `resolve_session_context_tenant`. A session that
        // RECORDED tenant B (the spawn input, else the `machine.json` active pin
        // threaded by the spawn site — the same source
        // `stamp_session_tenant` uses for the coord record + JWT slot) MUST
        // federate B's pool. The recorded value has to WIN over the
        // `paired_user.json::default_tenant_id` fallback (`resolve_tenant_id`);
        // that fallback following a DIFFERENT source is the exact coord/observable
        // split-brain B2 fixes. If the resolver ever ignores the threaded tenant
        // and consults the default instead, coord would record B while the
        // bridges pulled/pushed A — this assertion guards that.
        let recorded = Uuid::new_v4();
        assert_eq!(
            resolve_session_context_tenant(Some(recorded)),
            Some(recorded),
            "recorded session tenant must win over the paired-default fallback",
        );
    }
}
