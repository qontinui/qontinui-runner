//! System operations commands
//!
//! Commands for system-level operations like error handling, updates, and folder opening.

use crate::error::UserFacingError;
use std::process::Command;
use tauri::AppHandle;
use tracing::{error, info};

use super::super::CommandResponse;

/// Handle and emit user-facing errors.
///
/// Logs the error and emits it to the frontend for display.
///
/// # Arguments
/// * `error` - The user-facing error to handle
/// * `app_handle` - Tauri application handle for event emission
///
/// # Returns
/// * `Ok(())` - Success
/// * `Err(String)` - Error if event emission fails
#[tauri::command]
pub fn handle_error(error: UserFacingError, app_handle: AppHandle) -> Result<(), String> {
    error!("User-facing error: {:?}", error);

    // Emit through the shared sink rather than inline, so a backend module
    // that needs the same card does not have to invoke this command for its
    // side effect — and so the two cannot drift about the channel name. The
    // failure is still propagated to the caller, exactly as before.
    crate::error::emit_user_facing_error(&app_handle, &error)
}

/// Check for application updates.
///
/// In release builds, checks for available updates via Tauri updater.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success with update availability information
/// * `Err(String)` - Error if update check fails
#[tauri::command]
pub async fn check_for_updates(app_handle: AppHandle) -> Result<CommandResponse, String> {
    run_update_op(UpdateOp::Check, || check_for_updates_inner(app_handle)).await
}

/// Which updater operation [`run_update_op`] is guarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateOp {
    Check,
    Install,
}

/// The refusal both commands answer while the tenant's `egress_update_check`
/// switch is off (plan 2026-10-10-spec-front-end-phase-9-generic-boundary,
/// Phase 7). `success: true` with `status: "egress_off"`, never an error:
/// `AutoUpdateChecker.tsx` and `UpdateSettings.tsx` render it as "Update
/// checks are off for this project".
fn update_egress_refusal(op: UpdateOp, source: crate::egress::LevelSource) -> CommandResponse {
    let mut data = serde_json::json!({
        "status": "egress_off",
        "flow": crate::egress::Flow::UpdateCheck.key(),
        "source": source.as_str(),
        "current_version": env!("CARGO_PKG_VERSION"),
    });
    match op {
        UpdateOp::Check => data["available"] = serde_json::json!(false),
        UpdateOp::Install => data["installed"] = serde_json::json!(false),
    }
    CommandResponse {
        success: op == UpdateOp::Check,
        message: Some("Update checks are off for this project".to_string()),
        data: Some(data),
    }
}

/// Run an updater operation only when the switch allows it. The check runs
/// BEFORE `op` is called, so with the switch off the updater is never built
/// and no request leaves the machine.
async fn run_update_op<F, Fut>(op_kind: UpdateOp, op: F) -> Result<CommandResponse, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<CommandResponse, String>>,
{
    let verdict = crate::egress::permit(crate::egress::Flow::UpdateCheck);
    if !crate::egress::permit_or_count(crate::egress::Flow::UpdateCheck) {
        info!(
            "Update {} skipped: egress_update_check is off (source={})",
            if op_kind == UpdateOp::Check {
                "check"
            } else {
                "install"
            },
            verdict.source.as_str()
        );
        return Ok(update_egress_refusal(op_kind, verdict.source));
    }
    op().await
}

async fn check_for_updates_inner(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
    info!("Checking for updates");

    #[cfg(not(debug_assertions))]
    {
        use tauri_plugin_updater::UpdaterExt;

        match app_handle.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    info!("Update available: {}", update.version);
                    Ok(CommandResponse {
                        success: true,
                        message: Some(format!("Update available: {}", update.version)),
                        data: Some(serde_json::json!({
                            "available": true,
                            "version": update.version.to_string(),
                            "current_version": env!("CARGO_PKG_VERSION"),
                            "notes": update.body,
                        })),
                    })
                }
                Ok(None) => {
                    info!("No updates available");
                    Ok(CommandResponse {
                        success: true,
                        message: Some("No updates available".to_string()),
                        data: Some(serde_json::json!({
                            "available": false,
                            "current_version": env!("CARGO_PKG_VERSION"),
                        })),
                    })
                }
                Err(e) => {
                    error!("Failed to check for updates: {}", e);
                    Err(format!("Failed to check for updates: {}", e))
                }
            },
            Err(e) => {
                error!("Failed to build updater: {}", e);
                Err(format!("Failed to build updater: {}", e))
            }
        }
    }

    #[cfg(debug_assertions)]
    {
        info!("Update check skipped in development mode");
        Ok(CommandResponse {
            success: true,
            message: Some("Update check disabled in development".to_string()),
            data: Some(serde_json::json!({
                "available": false,
                "current_version": env!("CARGO_PKG_VERSION"),
                "development": true,
            })),
        })
    }
}

/// Download and install an available update.
///
/// In release builds, downloads and installs the update, then restarts the application.
/// In debug builds, returns a development mode message.
///
/// # Arguments
/// * `app_handle` - Tauri application handle for updater access
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message (note: app will restart on success)
/// * `Err(String)` - Error if update installation fails
#[tauri::command]
pub async fn install_update(app_handle: AppHandle) -> Result<CommandResponse, String> {
    run_update_op(UpdateOp::Install, || install_update_inner(app_handle)).await
}

async fn install_update_inner(
    #[allow(unused_variables)] app_handle: AppHandle,
) -> Result<CommandResponse, String> {
    info!("Installing update");

    #[cfg(not(debug_assertions))]
    {
        use tauri_plugin_updater::UpdaterExt;

        match app_handle.updater_builder().build() {
            Ok(updater) => match updater.check().await {
                Ok(Some(update)) => {
                    info!("Downloading update version {}", update.version);

                    // Download and install the update
                    match update
                        .download_and_install(|_chunk_length, _content_length| {}, || {})
                        .await
                    {
                        Ok(_) => {
                            info!("Update installed successfully, restarting application");
                            Ok(CommandResponse {
                                success: true,
                                message: Some(
                                    "Update installed. The application will restart.".to_string(),
                                ),
                                data: Some(serde_json::json!({
                                    "installed": true,
                                    "version": update.version.to_string(),
                                })),
                            })
                        }
                        Err(e) => {
                            error!("Failed to install update: {}", e);
                            Err(format!("Failed to install update: {}", e))
                        }
                    }
                }
                Ok(None) => {
                    info!("No update available to install");
                    Ok(CommandResponse {
                        success: false,
                        message: Some("No update available".to_string()),
                        data: Some(serde_json::json!({
                            "installed": false,
                        })),
                    })
                }
                Err(e) => {
                    error!("Failed to check for updates: {}", e);
                    Err(format!("Failed to check for updates: {}", e))
                }
            },
            Err(e) => {
                error!("Failed to build updater: {}", e);
                Err(format!("Failed to build updater: {}", e))
            }
        }
    }

    #[cfg(debug_assertions)]
    {
        info!("Update installation skipped in development mode");
        Ok(CommandResponse {
            success: false,
            message: Some("Updates are disabled in development mode".to_string()),
            data: Some(serde_json::json!({
                "installed": false,
                "development": true,
            })),
        })
    }
}

/// Open a folder in the system file explorer.
///
/// Uses platform-specific commands (explorer/open/xdg-open).
///
/// # Arguments
/// * `path` - The folder path to open
///
/// # Returns
/// * `Ok(CommandResponse)` - Success message
/// * `Err(String)` - Error if path doesn't exist or open fails
#[tauri::command]
pub fn open_folder(path: String) -> Result<CommandResponse, String> {
    info!("Opening folder: {}", path);

    // Check if path exists
    if !std::path::Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    #[cfg(target_os = "windows")]
    {
        // console-ok: a GUI file manager, not a console program.
        Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "macos")]
    {
        // console-ok: macOS reveal — never reached on Windows.
        Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        // console-ok: Linux reveal — never reached on Windows.
        Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(CommandResponse {
        success: true,
        message: Some(format!("Opened folder: {}", path)),
        data: None,
    })
}

/// The registered flow test for the update check
/// (`crate::egress::FLOW_TESTS`). The updater needs a Tauri `AppHandle`, so
/// the test drives [`run_update_op`] — the wrapper both commands go through —
/// with an operation that requests a manifest from a counting loopback
/// "release server" in place of the updater.
#[cfg(test)]
pub(crate) mod egress_tests {
    use super::*;
    use crate::egress::test_support::{pin, ConnCounter};
    use crate::egress::{Flow, Level};

    async fn check_once(level: Level, op_kind: UpdateOp) -> (usize, CommandResponse) {
        let counter = ConnCounter::start();
        let url = format!("{}/latest.json", counter.http_base());
        let _pin = pin(Flow::UpdateCheck, level);
        let resp = run_update_op(op_kind, || async move {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let _ = client.get(&url).send().await;
            Ok(CommandResponse {
                success: true,
                message: None,
                data: Some(serde_json::json!({"available": false})),
            })
        })
        .await
        .unwrap();
        (
            counter.wait_for(1, std::time::Duration::from_millis(300)),
            resp,
        )
    }

    #[tokio::test]
    pub(crate) async fn update_check_pinned_off_makes_zero_connections() {
        let (connects, resp) = check_once(Level::Off, UpdateOp::Check).await;
        assert_eq!(
            connects, 0,
            "the updater must not be reached with the switch off"
        );
        let data = resp.data.unwrap();
        assert_eq!(data["status"], "egress_off");
        assert_eq!(data["available"], false);
        assert!(resp.success, "a refusal is not an error");

        let (connects, resp) = check_once(Level::Off, UpdateOp::Install).await;
        assert_eq!(connects, 0, "install refuses the same way");
        assert!(!resp.success);
        assert_eq!(resp.data.unwrap()["installed"], false);
    }

    #[tokio::test]
    pub(crate) async fn update_check_pinned_on_makes_a_connection() {
        let (connects, resp) = check_once(Level::On, UpdateOp::Check).await;
        assert!(connects >= 1, "with the switch on the operation runs");
        assert_ne!(resp.data.unwrap()["status"], "egress_off");
    }

    #[test]
    fn both_commands_route_through_the_gate() {
        let src = include_str!("system_ops.rs")
            .split_once("pub(crate) mod egress_tests")
            .unwrap()
            .0;
        assert!(
            src.contains("run_update_op(UpdateOp::Check, || check_for_updates_inner(app_handle))")
        );
        assert!(
            src.contains("run_update_op(UpdateOp::Install, || install_update_inner(app_handle))")
        );
    }
}
