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

/// Build a process config JSON value.
fn build_process_config(
    name: &str,
    framework_label: &str,
    command: &str,
    args: &[&str],
    cwd: &str,
    health_port: Option<u16>,
    parser: &str,
    category: &str,
) -> Value {
    let mut config = serde_json::json!({
        "id": Uuid::new_v4().to_string(),
        "name": format!("{} ({})", name, framework_label),
        "command": command,
        "args": args,
        "cwd": cwd,
        "parser": parser,
        "category": category,
        "auto_start": false,
        "enabled": true,
        "buffer_size": 2000,
    });
    if let Some(port) = health_port {
        config["health_port"] = serde_json::json!(port);
    }
    config
}

/// Detect the specific framework by inspecting manifest files.
///
/// Returns a framework key like "nextjs", "fastapi", "vite", etc.
/// Falls back to a generic key like "node_dev" or empty string.
fn detect_framework(path: &str, generic_type: &str) -> String {
    let dir = std::path::Path::new(path);

    match generic_type {
        "node" => {
            let pkg_path = dir.join("package.json");
            if let Ok(contents) = std::fs::read_to_string(&pkg_path) {
                if let Ok(pkg) = serde_json::from_str::<Value>(&contents) {
                    let has_dep = |name: &str| -> bool {
                        pkg.get("dependencies").and_then(|d| d.get(name)).is_some()
                            || pkg
                                .get("devDependencies")
                                .and_then(|d| d.get(name))
                                .is_some()
                    };

                    // Check for specific frameworks (order matters)
                    if has_dep("next") {
                        return "nextjs".to_string();
                    }
                    if has_dep("@docusaurus/core") {
                        return "docusaurus".to_string();
                    }
                    if has_dep("expo") || has_dep("react-native") {
                        return "react_native".to_string();
                    }
                    if has_dep("express") {
                        return "express".to_string();
                    }
                    if has_dep("@nestjs/core") {
                        return "nestjs".to_string();
                    }
                    if has_dep("vite") {
                        return "vite".to_string();
                    }

                    // Check for runnable scripts (dev or start)
                    if let Some(scripts) = pkg.get("scripts").and_then(|s| s.as_object()) {
                        // Skip pure build/library packages
                        let is_build_only = |script: &str| -> bool {
                            let s = script.to_lowercase();
                            s.contains("tsup")
                                || s.contains("tsc")
                                || s.contains("rollup")
                                || s.contains("esbuild")
                        };

                        if let Some(dev) = scripts.get("dev").and_then(|v| v.as_str()) {
                            if !is_build_only(dev) {
                                return "node_dev".to_string();
                            }
                        }
                        if let Some(start) = scripts.get("start").and_then(|v| v.as_str()) {
                            if !is_build_only(start) {
                                return "node_start".to_string();
                            }
                        }
                    }
                }
            }
            String::new()
        }
        "python" => {
            // Check pyproject.toml for framework dependencies
            let pyproject_path = dir.join("pyproject.toml");
            if let Ok(contents) = std::fs::read_to_string(&pyproject_path) {
                let contents_lower = contents.to_lowercase();
                if contents_lower.contains("fastapi") {
                    return "fastapi".to_string();
                }
                if contents_lower.contains("django") {
                    return "django".to_string();
                }
                if contents_lower.contains("flask") {
                    return "flask".to_string();
                }
            }
            // Check requirements.txt as fallback
            let req_path = dir.join("requirements.txt");
            if let Ok(contents) = std::fs::read_to_string(&req_path) {
                let contents_lower = contents.to_lowercase();
                if contents_lower.contains("fastapi") {
                    return "fastapi".to_string();
                }
                if contents_lower.contains("django") {
                    return "django".to_string();
                }
                if contents_lower.contains("flask") {
                    return "flask".to_string();
                }
            }
            String::new()
        }
        "rust" => "rust".to_string(),
        "go" => "go".to_string(),
        "flutter" => "flutter".to_string(),
        _ => String::new(),
    }
}

/// Generate process configs for a detected framework.
fn configs_for_framework(name: &str, path: &str, framework: &str) -> Vec<Value> {
    match framework {
        "nextjs" => vec![build_process_config(
            name,
            "Next.js",
            "npm",
            &["run", "dev"],
            path,
            Some(3000),
            "javascript",
            "frontend",
        )],
        "fastapi" => vec![build_process_config(
            name,
            "FastAPI",
            "python",
            &["-m", "uvicorn", "main:app", "--reload"],
            path,
            Some(8000),
            "python",
            "backend",
        )],
        "express" => vec![build_process_config(
            name,
            "Express",
            "npm",
            &["start"],
            path,
            Some(3000),
            "javascript",
            "backend",
        )],
        "nestjs" => vec![build_process_config(
            name,
            "NestJS",
            "npm",
            &["run", "start:dev"],
            path,
            Some(3000),
            "javascript",
            "backend",
        )],
        "django" => vec![build_process_config(
            name,
            "Django",
            "python",
            &["manage.py", "runserver"],
            path,
            Some(8000),
            "python",
            "backend",
        )],
        "vite" => vec![build_process_config(
            name,
            "Vite",
            "npm",
            &["run", "dev"],
            path,
            Some(5173),
            "javascript",
            "frontend",
        )],
        "rust" => vec![build_process_config(
            name,
            "Cargo",
            "cargo",
            &["run"],
            path,
            None,
            "rust",
            "backend",
        )],
        "flask" => vec![build_process_config(
            name,
            "Flask",
            "python",
            &["-m", "flask", "run"],
            path,
            Some(5000),
            "python",
            "backend",
        )],
        "node_dev" => vec![build_process_config(
            name,
            "npm dev",
            "npm",
            &["run", "dev"],
            path,
            None,
            "javascript",
            "general",
        )],
        "node_start" => vec![build_process_config(
            name,
            "npm start",
            "npm",
            &["start"],
            path,
            None,
            "javascript",
            "general",
        )],
        "go" => vec![build_process_config(
            name,
            "Go",
            "go",
            &["run", "."],
            path,
            None,
            "generic",
            "backend",
        )],
        "react_native" => vec![build_process_config(
            name,
            "Expo",
            "npx",
            &["expo", "start"],
            path,
            Some(8081),
            "javascript",
            "mobile",
        )],
        "docusaurus" => vec![build_process_config(
            name,
            "Docusaurus",
            "npm",
            &["start"],
            path,
            Some(3000),
            "javascript",
            "frontend",
        )],
        "flutter" => vec![build_process_config(
            name,
            "Flutter",
            "flutter",
            &["run"],
            path,
            None,
            "generic",
            "mobile",
        )],
        _ => vec![],
    }
}

/// Scan immediate subdirectories for additional runnable manifests (monorepo support).
fn scan_subdir_manifests(project_path: &str, project_name: &str) -> Vec<Value> {
    let dir = std::path::Path::new(project_path);
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return results,
    };

    let skip_dirs = [
        "node_modules",
        "target",
        "dist",
        "build",
        "__pycache__",
        ".next",
        "venv",
        ".venv",
        "e2e",
        "test",
        "tests",
        "__tests__",
        "examples",
        "packages",
    ];

    let manifests: &[(&str, &str)] = &[
        ("package.json", "node"),
        ("pyproject.toml", "python"),
        ("Cargo.toml", "rust"),
        ("go.mod", "go"),
    ];

    for entry in entries.flatten() {
        let sub_path = entry.path();
        if !sub_path.is_dir() {
            continue;
        }

        let sub_name = sub_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        if sub_name.starts_with('.') || skip_dirs.contains(&sub_name) {
            continue;
        }

        for &(manifest, generic_type) in manifests {
            if sub_path.join(manifest).exists() {
                let sub_path_str = sub_path.to_string_lossy().to_string();
                let framework = detect_framework(&sub_path_str, generic_type);
                if !framework.is_empty() {
                    let display_name = format!("{}/{}", project_name, sub_name);
                    let configs = configs_for_framework(&display_name, &sub_path_str, &framework);
                    results.extend(configs);
                }
                break;
            }
        }
    }

    results
}

/// Suggest process configurations based on detected project frameworks.
///
/// Inspects manifest files (package.json, pyproject.toml, Cargo.toml) to detect
/// the specific framework and suggest appropriate dev commands. For monorepos,
/// also scans immediate subdirectories for additional runnable apps.
#[tauri::command]
pub async fn suggest_process_configs_for_setup(projects: Vec<Value>) -> Result<Vec<Value>, String> {
    info!(
        "Setup wizard: suggesting process configs for {} projects",
        projects.len()
    );

    // Exclude the runner's own project directory.
    let runner_dir = std::env::current_exe().ok().and_then(|exe| {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d.join("package.json").exists() && d.join("src-tauri").exists() {
                return Some(d.to_string_lossy().replace('\\', "/"));
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    });

    let mut suggestions = Vec::new();

    for project in &projects {
        let path = project
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let generic_type = project
            .get("type")
            .or_else(|| project.get("framework"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let name = project
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // Skip the runner's own project.
        if let Some(ref runner) = runner_dir {
            let norm_path = path.replace('\\', "/");
            if norm_path == *runner {
                info!("Skipping runner's own project: {}", name);
                continue;
            }
        }

        // Detect framework from manifest files.
        let framework = detect_framework(path, generic_type);

        // Generate configs for the project root.
        let root_configs = if framework.is_empty() {
            vec![]
        } else {
            configs_for_framework(name, path, &framework)
        };

        // Scan subdirectories for monorepo apps.
        let sub_configs = scan_subdir_manifests(path, name);

        if !sub_configs.is_empty() {
            // If a subdir has the same framework as the root, the root is likely a
            // workspace config rather than a runnable app — suppress it.
            let root_duplicated = !framework.is_empty()
                && sub_configs.iter().any(|c| {
                    c.get("name")
                        .and_then(|n| n.as_str())
                        .map(|n| n.contains(&framework))
                        .unwrap_or(false)
                });

            suggestions.extend(sub_configs);
            if !root_duplicated {
                suggestions.extend(root_configs);
            }
        } else {
            suggestions.extend(root_configs);
        }
    }

    Ok(suggestions)
}
