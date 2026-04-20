//! Restate server lifecycle management.
//!
//! Handles binary discovery/download, ProcessConfig creation, health checking,
//! service endpoint registration, and auto-recovery watchdog.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

use crate::error_monitor::types::ParserType;
use crate::process_capture::{ProcessCaptureManager, ProcessConfig};

use super::config::RestateSettings;

/// Expected binary name for the Restate server.
#[cfg(target_os = "windows")]
const RESTATE_BINARY_NAME: &str = "restate-server.exe";
#[cfg(not(target_os = "windows"))]
const RESTATE_BINARY_NAME: &str = "restate-server";

/// GitHub release URL template for downloading the Restate server binary.
const RESTATE_RELEASE_URL_TEMPLATE: &str =
    "https://github.com/restatedev/restate/releases/download/v{version}/restate-server-{target}";

/// Resolve the path to the Restate server binary.
///
/// Priority:
/// 1. Custom `binary_path` from settings
/// 2. Managed binary in `{app_data}/restate/bin/restate-server[.exe]`
/// 3. Binary on system PATH
fn resolve_binary_path(config: &RestateSettings, app_data_dir: &Path) -> Option<PathBuf> {
    // 1. Custom path from settings
    if let Some(ref custom_path) = config.binary_path {
        let path = PathBuf::from(custom_path);
        if path.exists() {
            info!("Using custom Restate binary: {}", path.display());
            return Some(path);
        }
        warn!(
            "Custom Restate binary path does not exist: {}",
            path.display()
        );
    }

    // 2. Managed binary in app data
    let managed_path = config
        .resolve_binary_dir(app_data_dir)
        .join(RESTATE_BINARY_NAME);
    if managed_path.exists() {
        debug!("Using managed Restate binary: {}", managed_path.display());
        return Some(managed_path);
    }

    // 3. System PATH
    if let Ok(path) = which_binary(RESTATE_BINARY_NAME) {
        info!("Using system Restate binary: {}", path.display());
        return Some(path);
    }

    None
}

/// Look up a binary on the system PATH.
fn which_binary(name: &str) -> Result<PathBuf, String> {
    // Simple PATH lookup without the `which` crate
    let path_var = std::env::var("PATH").map_err(|e| format!("PATH not set: {}", e))?;
    let separator = if cfg!(windows) { ';' } else { ':' };
    for dir in path_var.split(separator) {
        let candidate = PathBuf::from(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!("Binary '{}' not found on PATH", name))
}

/// Get the platform-specific target triple for Restate binary downloads.
///
/// Returns an `Err` with actionable guidance on platforms where the Restate
/// project does not publish prebuilt binaries (notably Windows).
fn download_target() -> Result<&'static str, String> {
    #[cfg(target_os = "windows")]
    {
        Err("Restate does not publish Windows binaries. To use Restate on Windows, either:\n\
             (a) set RestateSettings.binary_path to a locally-built restate-server.exe, or\n\
             (b) set RestateSettings.external_admin_url and external_ingress_url to a Docker- or WSL-hosted server.\n\
             See https://github.com/restatedev/restate/releases for platform availability.".to_string())
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Ok("x86_64-unknown-linux-musl")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Ok("aarch64-unknown-linux-musl")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Ok("x86_64-apple-darwin")
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Ok("aarch64-apple-darwin")
    }
}

/// Validate the installed Restate binary version.
async fn validate_binary_version(binary_path: &Path, expected_version: &str) -> bool {
    match tokio::process::Command::new(binary_path)
        .arg("--version")
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout);
            let is_match = version_str.contains(expected_version);
            if !is_match {
                debug!(
                    "Restate binary version mismatch: got '{}', expected '{}'",
                    version_str.trim(),
                    expected_version
                );
            }
            is_match
        }
        Ok(output) => {
            warn!(
                "Restate --version failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            false
        }
        Err(e) => {
            warn!("Failed to run Restate binary: {}", e);
            false
        }
    }
}

/// Download the Restate server binary to the managed directory.
async fn download_restate_binary(
    config: &RestateSettings,
    app_data_dir: &Path,
) -> Result<PathBuf, String> {
    let target = download_target()?;
    let url = RESTATE_RELEASE_URL_TEMPLATE
        .replace("{version}", &config.target_version)
        .replace("{target}", target);

    info!(
        "Downloading Restate server v{} from {}",
        config.target_version, url
    );

    let binary_dir = config.resolve_binary_dir(app_data_dir);
    tokio::fs::create_dir_all(&binary_dir)
        .await
        .map_err(|e| format!("Failed to create Restate binary directory: {}", e))?;

    let dest_path = binary_dir.join(RESTATE_BINARY_NAME);

    let response = reqwest::get(&url)
        .await
        .map_err(|e| format!("Failed to download Restate binary: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Restate binary download failed with status {}: {}",
            response.status(),
            url
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read Restate binary response: {}", e))?;

    tokio::fs::write(&dest_path, &bytes)
        .await
        .map_err(|e| format!("Failed to write Restate binary: {}", e))?;

    // Make executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = tokio::fs::metadata(&dest_path)
            .await
            .map_err(|e| format!("Failed to read permissions: {}", e))?
            .permissions();
        perms.set_mode(0o755);
        tokio::fs::set_permissions(&dest_path, perms)
            .await
            .map_err(|e| format!("Failed to set executable permissions: {}", e))?;
    }

    info!(
        "Restate server v{} downloaded to {}",
        config.target_version,
        dest_path.display()
    );
    Ok(dest_path)
}

/// Ensure the Restate binary is available, downloading if necessary.
///
/// Returns the path to the binary, or an error if it cannot be obtained.
/// When `config.is_external()` is true this returns an error immediately — the
/// caller treats that as non-fatal and skips local spawning.
pub async fn ensure_restate_binary(
    config: &RestateSettings,
    app_data_dir: &Path,
) -> Result<PathBuf, String> {
    if config.is_external() {
        return Err(
            "Restate is configured to use an external server (external_admin_url/external_ingress_url). \
             Skipping managed binary resolution."
                .to_string(),
        );
    }

    // Check existing binary
    if let Some(path) = resolve_binary_path(config, app_data_dir) {
        // Validate version if it's a managed binary (not custom or system)
        if config.binary_path.is_none() {
            if validate_binary_version(&path, &config.target_version).await {
                return Ok(path);
            }
            info!("Managed Restate binary version mismatch, re-downloading");
        } else {
            return Ok(path);
        }
    }

    // Download if auto_download is enabled
    if config.auto_download {
        download_restate_binary(config, app_data_dir).await
    } else {
        Err("Restate binary not found and auto_download is disabled. \
             Install restate-server manually or set restate.binary_path in settings."
            .to_string())
    }
}

/// Build a `ProcessConfig` for the Restate server subprocess.
pub fn build_restate_process_config(
    config: &RestateSettings,
    binary_path: &Path,
    app_data_dir: &Path,
) -> ProcessConfig {
    let data_dir = config.resolve_data_dir(app_data_dir);

    // Write the server config file
    let config_dir = app_data_dir.join("restate");
    let config_file = config_dir.join("restate.toml");

    // Generate config synchronously (it's just string formatting)
    let config_content = config.generate_server_config(app_data_dir);
    // Write config file synchronously — we're called during startup
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        warn!("Failed to create Restate config directory: {}", e);
    }
    if let Err(e) = std::fs::write(&config_file, &config_content) {
        warn!("Failed to write Restate config: {}", e);
    }

    // Ensure data directory exists
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        warn!("Failed to create Restate data directory: {}", e);
    }

    ProcessConfig {
        id: "restate-server".to_string(),
        name: "Restate Server (Durable Execution)".to_string(),
        command: binary_path.to_string_lossy().to_string(),
        args: vec![
            "--config-file".to_string(),
            config_file.to_string_lossy().to_string(),
            "--cluster-name".to_string(),
            "qontinui-local".to_string(),
        ],
        cwd: config_dir.to_string_lossy().to_string(),
        env: HashMap::new(),
        health_port: Some(config.admin_port),
        parser: ParserType::Generic,
        auto_start: true,
        category: "infrastructure".to_string(),
        buffer_size: 2000,
        enabled: true,
        ignore_patterns: vec![],
        start_group: 0,
        dev_only: false,
        rebuild_enabled: false,
        build_command: None,
        build_args: vec![],
    }
}

/// Check if the Restate server is healthy by querying the admin API.
///
/// Targets the local admin port. For external servers use
/// [`is_restate_healthy_for`] which consults `external_admin_url`.
pub async fn is_restate_healthy(admin_port: u16) -> bool {
    probe_health(&format!("http://127.0.0.1:{}", admin_port)).await
}

/// Check Restate health using the effective admin URL from settings.
///
/// When `external_admin_url` is set this probes the external server; otherwise
/// it probes the local admin port.
pub async fn is_restate_healthy_for(config: &RestateSettings) -> bool {
    probe_health(&config.admin_url()).await
}

async fn probe_health(admin_base_url: &str) -> bool {
    let url = format!("{}/health", admin_base_url.trim_end_matches('/'));
    match reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(2))
        .send()
        .await
    {
        Ok(response) => response.status().is_success(),
        Err(_) => false,
    }
}

/// Register the runner's service endpoint with the Restate server.
///
/// This tells Restate where to find the QontinuiWorkflow and WorkflowStateObject
/// service handlers. Must be called after both Restate and the service endpoint are ready.
///
/// Targets `settings.admin_url()` which honors `external_admin_url` when set.
pub async fn register_service_endpoint(
    settings: &RestateSettings,
    service_endpoint_port: u16,
) -> Result<(), String> {
    let admin_url = format!("{}/deployments", settings.admin_url().trim_end_matches('/'));
    let service_url = format!("http://127.0.0.1:{}", service_endpoint_port);

    info!(
        "Registering Restate service endpoint: {} → {}",
        admin_url, service_url
    );

    let client = reqwest::Client::new();
    let response = client
        .post(&admin_url)
        .json(&serde_json::json!({
            "uri": service_url
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("Failed to register Restate deployment: {}", e))?;

    if response.status().is_success() {
        info!("Restate service endpoint registered successfully");
        Ok(())
    } else {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        // 409 Conflict means already registered — that's fine
        if status.as_u16() == 409 {
            info!("Restate service endpoint already registered (409 Conflict)");
            Ok(())
        } else {
            Err(format!(
                "Restate deployment registration failed ({}): {}",
                status, body
            ))
        }
    }
}

/// Spawn a background watchdog that monitors Restate health and restarts it if needed.
///
/// No-op when `config.is_external()` is true — the runner does not own the
/// lifecycle of an external Restate server.
pub fn spawn_restate_watchdog(manager: Arc<ProcessCaptureManager>, config: RestateSettings) {
    if config.is_external() {
        info!(
            "Restate configured with external URLs ({}); skipping managed-binary watchdog",
            config.admin_url()
        );
        return;
    }

    tokio::spawn(async move {
        let check_interval = Duration::from_secs(30);
        let mut consecutive_failures = 0u32;
        let max_failures_before_restart = 3;

        loop {
            tokio::time::sleep(check_interval).await;

            if is_restate_healthy_for(&config).await {
                if consecutive_failures > 0 {
                    info!(
                        "Restate server recovered (was unhealthy for {} checks)",
                        consecutive_failures
                    );
                    consecutive_failures = 0;
                }
            } else {
                consecutive_failures += 1;
                warn!(
                    "Restate server unhealthy (consecutive failures: {}/{})",
                    consecutive_failures, max_failures_before_restart
                );

                if consecutive_failures >= max_failures_before_restart {
                    error!(
                        "Restate server unresponsive after {} checks, attempting restart",
                        consecutive_failures
                    );
                    match manager.restart_process("restate-server").await {
                        Ok(_) => {
                            info!("Restate server restart initiated");
                            consecutive_failures = 0;
                            // Wait for restart to complete
                            tokio::time::sleep(Duration::from_secs(10)).await;
                        }
                        Err(e) => {
                            error!("Failed to restart Restate server: {}", e);
                        }
                    }
                }
            }
        }
    });
}
