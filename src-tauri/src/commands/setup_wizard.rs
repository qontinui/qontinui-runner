//! Setup Wizard Commands
//!
//! Provides Tauri commands for the first-launch setup wizard.
//! Discovery operations call the qontinui-setup-mcp CLI via subprocess
//! (works offline, no AI connection required).

use crate::settings::{self, AiProvider, CliExecutionMode, GlobalLogSource};
use serde_json::Value;
use std::process::Command;
use tracing::{info, warn};
use uuid::Uuid;

// ============================================================================
// Setup Status
// ============================================================================

/// Check if the first-launch setup wizard has been completed
#[tauri::command]
pub fn check_setup_completed() -> Result<bool, String> {
    Ok(settings::get_setup_completed())
}

/// Mark the setup wizard as completed
#[tauri::command]
pub fn complete_setup() -> Result<(), String> {
    info!("Setup wizard completed");
    settings::save_setup_completed(true)
}

// ============================================================================
// Python CLI Subprocess Helpers
// ============================================================================

/// Find the qontinui-setup-mcp source directory by searching parent directories.
///
/// Looks for a `qontinui-setup-mcp/src` directory containing the Python package.
/// Searches from the executable's location upward, then from the current working
/// directory upward. This handles both dev builds (where the exe is in
/// `src-tauri/target/debug`) and the standard workspace layout.
fn find_setup_mcp_src_dir() -> Option<std::path::PathBuf> {
    // Collect candidate root directories to search from
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();

    // Start from the executable's directory and walk up
    if let Ok(exe_path) = std::env::current_exe() {
        let mut dir = exe_path.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    // Also try from the current working directory upward
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = Some(cwd);
        while let Some(d) = dir {
            candidates.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }

    // Check each candidate for the setup-mcp package
    for candidate in &candidates {
        let src_dir = candidate.join("qontinui-setup-mcp").join("src");
        let module_init = src_dir.join("qontinui_setup_mcp").join("__init__.py");
        if module_init.exists() {
            info!("Found qontinui-setup-mcp source at: {}", src_dir.display());
            return Some(src_dir);
        }
    }

    // Also check QONTINUI_PARENT_DIR env var if set
    if let Ok(parent_dir) = std::env::var("QONTINUI_PARENT_DIR") {
        let src_dir = std::path::PathBuf::from(&parent_dir)
            .join("qontinui-setup-mcp")
            .join("src");
        let module_init = src_dir.join("qontinui_setup_mcp").join("__init__.py");
        if module_init.exists() {
            info!(
                "Found qontinui-setup-mcp source via QONTINUI_PARENT_DIR: {}",
                src_dir.display()
            );
            return Some(src_dir);
        }
    }

    None
}

/// Run a qontinui-setup-mcp CLI command and return parsed JSON output.
///
/// Automatically sets PYTHONPATH to include the qontinui-setup-mcp source
/// directory so the module can be found without requiring pip installation.
fn run_setup_cli(args: &[&str]) -> Result<Value, String> {
    let mut cmd = Command::new("python");
    cmd.args(
        std::iter::once("-m")
            .chain(std::iter::once("qontinui_setup_mcp.cli"))
            .chain(args.iter().copied()),
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());

    // Set PYTHONPATH so the module can be found from source
    if let Some(src_dir) = find_setup_mcp_src_dir() {
        let src_dir_str = src_dir.to_string_lossy().to_string();
        // Prepend to existing PYTHONPATH if set, otherwise just use the src dir
        let python_path = match std::env::var("PYTHONPATH") {
            Ok(existing) if !existing.is_empty() => {
                format!("{};{}", src_dir_str, existing)
            }
            _ => src_dir_str,
        };
        info!("Setting PYTHONPATH for setup CLI: {}", python_path);
        cmd.env("PYTHONPATH", python_path);
    } else {
        warn!(
            "Could not find qontinui-setup-mcp source directory; \
             assuming module is installed via pip"
        );
    }

    let output = cmd
        .output()
        .map_err(|e| format!("Failed to run setup CLI: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("Setup CLI failed: {}", stderr);
        return Err(format!("Setup CLI error: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|e| format!("Failed to parse setup CLI output: {}", e))
}

// ============================================================================
// Discovery Commands (via Python subprocess)
// ============================================================================

/// Scan a workspace directory for software projects
#[tauri::command]
pub async fn scan_workspace_for_setup(
    path: String,
    max_depth: Option<u32>,
) -> Result<Value, String> {
    info!("Setup wizard: scanning workspace at {}", path);
    let depth = max_depth.unwrap_or(3).to_string();
    tokio::task::spawn_blocking(move || {
        run_setup_cli(&["scan_workspace", &path, "--max-depth", &depth])
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

/// Detect the framework used by a project
#[tauri::command]
pub async fn detect_project_framework_for_setup(project_path: String) -> Result<Value, String> {
    info!("Setup wizard: detecting framework at {}", project_path);
    tokio::task::spawn_blocking(move || run_setup_cli(&["detect_framework", &project_path]))
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

/// Suggest log sources for a project
#[tauri::command]
pub async fn suggest_log_sources_for_setup(project_path: String) -> Result<Value, String> {
    info!("Setup wizard: suggesting log sources for {}", project_path);
    tokio::task::spawn_blocking(move || run_setup_cli(&["suggest_log_sources", &project_path]))
        .await
        .map_err(|e| format!("Task join error: {}", e))?
}

/// Suggest workspace-level dev-log sources (.dev-logs/ directory)
#[tauri::command]
pub async fn suggest_workspace_sources_for_setup(workspace_path: String) -> Result<Value, String> {
    info!(
        "Setup wizard: scanning workspace dev-logs at {}",
        workspace_path
    );
    tokio::task::spawn_blocking(move || {
        run_setup_cli(&["suggest_workspace_sources", &workspace_path])
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?
}

// ============================================================================
// Save Commands
// ============================================================================

/// Save selected log sources from the setup wizard
#[tauri::command]
pub fn save_log_sources_from_setup(sources: Vec<Value>) -> Result<(), String> {
    info!("Setup wizard: saving {} log sources", sources.len());

    let mut current_settings = settings::get_global_log_source_settings();

    for source_value in sources {
        match serde_json::from_value::<GlobalLogSource>(source_value) {
            Ok(source) => {
                // Avoid duplicates by path
                if !current_settings
                    .sources
                    .iter()
                    .any(|s| s.path == source.path)
                {
                    current_settings.sources.push(source);
                }
            }
            Err(e) => {
                warn!("Setup wizard: skipping invalid log source: {}", e);
            }
        }
    }

    settings::save_global_log_source_settings(current_settings)
}

/// Save AI provider configuration from the setup wizard
#[tauri::command]
pub fn save_ai_provider_from_setup(
    provider: String,
    model: Option<String>,
    execution_mode: Option<String>,
) -> Result<(), String> {
    info!("Setup wizard: saving AI provider: {}", provider);

    let mut ai_settings = settings::get_ai_settings();

    let ai_provider = match provider.as_str() {
        "claude_cli" => AiProvider::ClaudeCli,
        "claude_api" => AiProvider::ClaudeApi,
        "gemini_cli" => AiProvider::GeminiCli,
        "gemini_api" => AiProvider::GeminiApi,
        _ => return Err(format!("Unknown AI provider: {}", provider)),
    };

    ai_settings.provider = ai_provider.clone();

    let exec_mode = execution_mode.as_deref().map(|m| match m {
        "windows_native" => CliExecutionMode::WindowsNative,
        "wsl" => CliExecutionMode::Wsl,
        "native" => CliExecutionMode::Native,
        _ => CliExecutionMode::Auto,
    });

    match ai_provider {
        AiProvider::ClaudeCli => {
            if let Some(mode) = exec_mode {
                ai_settings.claude_cli.execution_mode = mode;
            }
        }
        AiProvider::ClaudeApi => {
            if let Some(m) = model {
                ai_settings.claude_api.model = m;
            }
        }
        AiProvider::GeminiCli => {
            if let Some(mode) = exec_mode {
                ai_settings.gemini_cli.execution_mode = mode;
            }
            if let Some(m) = model {
                ai_settings.gemini_cli.model = m;
            }
        }
        AiProvider::GeminiApi => {
            if let Some(m) = model {
                ai_settings.gemini_api.model = m;
            }
        }
    }

    settings::save_ai_settings(ai_settings)
}

// ============================================================================
// Process Config Suggestions
// ============================================================================

/// Suggest process configurations based on detected project frameworks.
///
/// Pure Rust mapping — no Python subprocess needed.
#[tauri::command]
pub async fn suggest_process_configs_for_setup(projects: Vec<Value>) -> Result<Vec<Value>, String> {
    info!(
        "Setup wizard: suggesting process configs for {} projects",
        projects.len()
    );

    let mut suggestions = Vec::new();

    for project in &projects {
        let path = project
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let framework = project
            .get("type")
            .or_else(|| project.get("framework"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let name = project
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let configs = match framework {
            "nextjs" | "next" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Next.js)", name),
                "command": "npm",
                "args": ["run", "dev"],
                "cwd": path,
                "health_port": 3000,
                "parser": "javascript",
                "category": "frontend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "fastapi" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (FastAPI)", name),
                "command": "python",
                "args": ["-m", "uvicorn", "main:app", "--reload"],
                "cwd": path,
                "health_port": 8000,
                "parser": "python",
                "category": "backend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "express" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Express)", name),
                "command": "npm",
                "args": ["start"],
                "cwd": path,
                "health_port": 3000,
                "parser": "javascript",
                "category": "backend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "django" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Django)", name),
                "command": "python",
                "args": ["manage.py", "runserver"],
                "cwd": path,
                "health_port": 8000,
                "parser": "python",
                "category": "backend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "react_vite" | "vite" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Vite)", name),
                "command": "npm",
                "args": ["run", "dev"],
                "cwd": path,
                "health_port": 5173,
                "parser": "javascript",
                "category": "frontend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "rust_cargo" | "rust" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Cargo)", name),
                "command": "cargo",
                "args": ["run"],
                "cwd": path,
                "parser": "rust",
                "category": "backend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            "flask" => vec![serde_json::json!({
                "id": Uuid::new_v4().to_string(),
                "name": format!("{} (Flask)", name),
                "command": "python",
                "args": ["-m", "flask", "run"],
                "cwd": path,
                "health_port": 5000,
                "parser": "python",
                "category": "backend",
                "auto_start": false,
                "enabled": true,
                "buffer_size": 2000,
            })],
            _ => vec![],
        };

        suggestions.extend(configs);
    }

    Ok(suggestions)
}
