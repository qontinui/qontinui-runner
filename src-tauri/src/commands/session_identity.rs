//! Session coord-identity **delivery gate** commands (plan
//! `2026-07-17-universal-coord-device-identity-for-any-session` §6).
//!
//! # What this toggles
//!
//! Coord device identity is not ambient. It is injected at spawn time by
//! `TerminalSession::apply_identity_seam`, whose sole non-test call site is the
//! runner's own PTY-terminal spawn — so a BARE session (a Windows Terminal /
//! Git Bash window, VS Code's integrated terminal, a cron-fired agent) gets no
//! `--mcp-config`, no nonce, and authenticates with neither a `device_id` nor an
//! `agent_id` claim, which is exactly the shape the device-scoped coord tools
//! reject.
//!
//! Turning this on materializes a persistent `claude.exe` shadow (the identity
//! shim — a *parent* of `claude`, the only process that can inject `--mcp-config`,
//! which the CLI reads exactly once at launch) into
//! `~/.qontinui/runner/identity-shim` and adds that dir to the USER PATH. A bare
//! `claude` then asks the live runner to mint device identity for its own cwd
//! and passes it on, fail-open.
//!
//! # ⚠ This IS a security gate — it is not a convenience toggle
//!
//! It is **one of two independent gates**, and neither alone grants a bare
//! session identity:
//!
//! 1. **Delivery gate (here).** Does a bare `claude` get wrapped at all? A
//!    persistent USER-PATH install of a `claude.exe` shadow is a deliberate
//!    operator act, and it is reversible in the same place it was enabled —
//!    which is exactly why a Settings toggle was chosen over a hand-dropped
//!    dotfile an operator cannot revoke what they cannot find.
//! 2. **Mint gate (runner-side).** Will a runner actually mint? The SAME-USER
//!    loopback handshake (`X-Qontinui-Loopback-Key`, matched against the
//!    owner-only `~/.qontinui/runner-loopback-key` this runner start wrote) AND
//!    `~/.qontinui/allow-session-coord-identity`. See
//!    `coord_mcp::session_identity_gate`. The former spawn-time master env flag
//!    was DELETED by plan
//!    `2026-08-24-headless-box-has-no-working-coord-credential-door` — it could
//!    be neither enabled nor revoked without restarting the runner.
//!
//! With this ON and the mint gate OFF, a bare `claude` POSTs once, is refused
//! with a typed 403, and launches the real CLI unchanged — the default posture
//! costs one refused loopback call and changes nothing else.

use serde::Serialize;
use tracing::info;

use crate::install_effects_producer::intercept::shim_materializer;

use super::CommandResponse;

/// Current state of the delivery gate, for the Settings panel.
#[derive(Serialize)]
struct SessionIdentityStatus {
    /// True only when the stub is materialized AND its dir is on the USER PATH.
    /// Either alone does nothing, so a partial install reads as `false`.
    installed: bool,
    /// The persistent identity-shim dir (shown so an operator can inspect or
    /// remove it by hand).
    dir: Option<String>,
}

/// Whether bare terminals on this machine get the identity shim.
#[tauri::command]
pub async fn session_identity_status() -> Result<CommandResponse, String> {
    let (installed, dir) = tokio::task::spawn_blocking(|| {
        (
            shim_materializer::persistent_identity_installed().unwrap_or(false),
            qontinui_runner_lib::profile_cli::identity_shim_dir()
                .map(|d| d.to_string_lossy().into_owned()),
        )
    })
    .await
    .map_err(|e| format!("session identity status task panicked: {e}"))?;

    Ok(CommandResponse {
        success: true,
        message: None,
        data: Some(
            serde_json::to_value(SessionIdentityStatus { installed, dir })
                .map_err(|e| format!("serialize session identity status: {e}"))?,
        ),
    })
}

/// Opt this machine in: materialize the persistent identity shim, then add its
/// dir to the USER PATH. Idempotent.
///
/// Order is load-bearing — materialize FIRST. A PATH entry pointing at a dir
/// with no stub in it is a silent no-op that reads as "enabled"; and
/// materialization is fail-closed precisely so that never ships.
#[tauri::command]
pub async fn session_identity_install() -> Result<CommandResponse, String> {
    let outcome = tokio::task::spawn_blocking(|| -> Result<_, String> {
        let dir = shim_materializer::materialize_persistent_identity()?;
        info!(
            "session_identity_install: materialized the persistent identity shim at {}",
            dir.display()
        );
        qontinui_runner_lib::profile_cli::install_identity_shim_on_user_path()
    })
    .await
    .map_err(|e| format!("session identity install task panicked: {e}"))??;

    info!(
        "session_identity_install: added={} already_present={} dir={}",
        outcome.added, outcome.already_present, outcome.dir
    );
    Ok(CommandResponse {
        success: true,
        message: Some(outcome.message.clone()),
        data: Some(
            serde_json::to_value(outcome)
                .map_err(|e| format!("serialize session identity outcome: {e}"))?,
        ),
    })
}

/// Opt this machine back out: remove the dir from the USER PATH, then delete it.
/// Idempotent, and the exact reverse of [`session_identity_install`].
///
/// Order is the mirror image — PATH entry OFF first, so no window exists in
/// which PATH names a dir whose stub has already been deleted.
#[tauri::command]
pub async fn session_identity_uninstall() -> Result<CommandResponse, String> {
    let outcome = tokio::task::spawn_blocking(|| -> Result<_, String> {
        let outcome = qontinui_runner_lib::profile_cli::uninstall_identity_shim_from_user_path()?;
        shim_materializer::remove_persistent_identity()?;
        Ok(outcome)
    })
    .await
    .map_err(|e| format!("session identity uninstall task panicked: {e}"))??;

    info!(
        "session_identity_uninstall: removed={} dir={}",
        outcome.already_present, outcome.dir
    );
    Ok(CommandResponse {
        success: true,
        message: Some(outcome.message.clone()),
        data: Some(
            serde_json::to_value(outcome)
                .map_err(|e| format!("serialize session identity outcome: {e}"))?,
        ),
    })
}
