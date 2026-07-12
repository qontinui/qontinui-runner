//! Setup Wizard Commands
//!
//! Provides Tauri commands for the first-launch setup wizard.
//! Discovery operations run natively in Rust via `commands::setup_discovery`
//! (works offline, no AI connection and no Python interpreter required).

use crate::error::AppError;
use crate::settings::{self, AiProvider, CliExecutionMode, GlobalLogSource};
use serde_json::Value;

use tracing::{info, warn};
use uuid::Uuid;

// ============================================================================
// Setup Status
// ============================================================================

/// Check if the first-launch setup wizard has been completed.
///
/// Auto-bypass for test runners: when the supervisor forwards
/// `QONTINUI_TEST_AUTO_LOGIN_EMAIL` (the same env var that drives the
/// existing test auto-login), report setup as complete regardless of
/// the persisted settings value. Temp runners always start with a
/// fresh profile (setup_completed=false) which previously blocked any
/// route-scoped UI Bridge testing behind the 7-step wizard. Reusing
/// the supervisor's existing auto-login env var means zero new
/// configuration on the spawn side — the same plumbing that auto-logs
/// also auto-skips.
///
/// Operators running the runner directly never set this env var, so
/// their wizard behaviour is unchanged. Manual dismissal is also
/// available via the `setup-wizard` UI Bridge component's `complete`
/// action (registered in `SetupWizard.tsx`).
#[tauri::command]
pub fn check_setup_completed() -> Result<bool, String> {
    if std::env::var("QONTINUI_TEST_AUTO_LOGIN_EMAIL")
        .ok()
        .filter(|v| !v.is_empty())
        .is_some()
    {
        return Ok(true);
    }
    Ok(settings::get_setup_completed())
}

/// Mark the setup wizard as completed
#[tauri::command]
pub fn complete_setup() -> Result<(), String> {
    info!("Setup wizard completed");
    settings::save_setup_completed(true)
}

// ============================================================================
// Discovery Commands (native Rust — no Python interpreter required)
// ============================================================================
//
// These were previously implemented by shelling out to the bundled
// `qontinui_setup_mcp` Python CLI, which required an end-user Python 3.12+
// interpreter and failed the first-launch scan on machines without one. The
// discovery logic now lives in `crate::commands::setup_discovery` as a faithful
// pure-Rust port, so the production wizard works out of the box.

/// Scan a workspace directory for software projects
#[tauri::command]
pub async fn scan_workspace_for_setup(
    path: String,
    max_depth: Option<u32>,
) -> Result<Value, String> {
    info!("Setup wizard: scanning workspace at {}", path);
    let depth = max_depth.unwrap_or(3);
    let projects = tokio::task::spawn_blocking(move || {
        crate::commands::setup_discovery::scan_workspace(&path, depth)
    })
    .await
    .map_err(|e| String::from(AppError::ProcessError(format!("Task join error: {}", e))))?;
    Ok(Value::Array(projects))
}

/// Detect the framework used by a project
#[tauri::command]
pub async fn detect_project_framework_for_setup(project_path: String) -> Result<Value, String> {
    info!("Setup wizard: detecting framework at {}", project_path);
    tokio::task::spawn_blocking(move || {
        crate::commands::setup_discovery::detect_framework(&project_path)
    })
    .await
    .map_err(|e| String::from(AppError::ProcessError(format!("Task join error: {}", e))))
}

/// Suggest log sources for a project
#[tauri::command]
pub async fn suggest_log_sources_for_setup(project_path: String) -> Result<Value, String> {
    info!("Setup wizard: suggesting log sources for {}", project_path);
    tokio::task::spawn_blocking(move || {
        crate::commands::setup_discovery::suggest_log_sources(&project_path)
    })
    .await
    .map_err(|e| String::from(AppError::ProcessError(format!("Task join error: {}", e))))
}

/// Suggest workspace-level dev-log sources (.dev-logs/ directory)
#[tauri::command]
pub async fn suggest_workspace_sources_for_setup(workspace_path: String) -> Result<Value, String> {
    info!(
        "Setup wizard: scanning workspace dev-logs at {}",
        workspace_path
    );
    tokio::task::spawn_blocking(move || {
        crate::commands::setup_discovery::suggest_workspace_sources(&workspace_path)
    })
    .await
    .map_err(|e| String::from(AppError::ProcessError(format!("Task join error: {}", e))))
}

// ============================================================================
// GitHub Repository Cloning (via the qontinui GitHub App)
// ============================================================================
//
// The setup wizard lets users pick a GitHub repository and clone it into a
// local folder. Instead of depending on a locally-authorized `gh` CLI (which an
// AI-driven runner usually won't have), we REUSE the GitHub App connection the
// user already made during qontinui onboarding.
//
// Flow: the runner calls the qontinui-web backend with its Cognito bearer; web
// resolves the operator's tenant and proxies to coord, which owns the GitHub
// App private key. coord lists the tenant's installation repos and mints a
// repo-scoped, `contents:read`, short-TTL token for a single clone. The runner
// never stores a long-lived GitHub credential — the clone token is used once and
// scrubbed from the cloned repo's `.git/config`.

/// Resolve the runner's Cognito bearer for authenticated web-backend calls.
/// `None` when the user isn't signed in (Tier 0/1 or no Cognito session).
async fn cognito_bearer() -> Option<String> {
    let auth_manager = crate::auth::AuthManager::new();
    crate::mcp::device_jwt_refresher::ensure_fresh_cognito_bearer(&auth_manager)
        .await
        .filter(|t| !t.trim().is_empty())
}

/// List the GitHub repositories the signed-in user's connected GitHub App
/// installation(s) can access, for the setup-wizard clone picker.
///
/// Returns `{ signed_in, connected, repos }`:
/// - `signed_in: false` — no Qontinui account session; the wizard shows a
///   sign-in hint (this is a normal state, not an error).
/// - `connected: false` — signed in, but the GitHub App isn't installed on any
///   of the user's orgs yet; the wizard shows a "connect your GitHub" CTA.
/// - otherwise `repos` is the list to render.
#[tauri::command]
pub async fn github_list_repos() -> Result<Value, String> {
    let Some(bearer) = cognito_bearer().await else {
        return Ok(serde_json::json!({
            "signed_in": false,
            "connected": false,
            "repos": [],
        }));
    };

    let url = format!(
        "{}/api/v1/operations/github/repos",
        crate::api_config::get_api_base_url()
    );
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(&bearer)
        .send()
        .await
        .map_err(|e| format!("Failed to reach Qontinui backend: {}", e))?;

    let status = resp.status();

    // P5 — record a REDACTED trace of the call we just made, so an agent can
    // read what the command actually did instead of reproducing it by hand.
    // Never the bearer (only its kind), never a body (only a type-shape).
    // Surfaced only via the observe tier, and only for opt-in commands.
    let (host, path) = crate::outbound_trace::split_url(&url);
    let mut trace = crate::outbound_trace::OutboundTrace {
        method: "GET".to_string(),
        host,
        path,
        status: status.as_u16(),
        // `cognito_bearer()` resolves the Cognito ACCESS token.
        bearer_kind: "access".to_string(),
        response_shape: None,
    };

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        // Error bodies can echo request context — record the status only.
        crate::outbound_trace::record("github_list_repos", trace);
        warn!("github_list_repos: backend returned {}: {}", status, body);
        return Err(format!("Backend returned {}: {}", status, body.trim()));
    }

    let mut body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse repo list: {}", e))?;

    trace.response_shape = Some(crate::outbound_trace::shape_of(&body));
    crate::outbound_trace::record("github_list_repos", trace);

    // The backend returns `{connected, repos}`; stamp `signed_in: true` so the
    // frontend has one uniform shape to branch on.
    if let Some(obj) = body.as_object_mut() {
        obj.insert("signed_in".to_string(), Value::Bool(true));
    }
    Ok(body)
}

/// Clone a GitHub repository into a local destination folder, reusing the user's
/// GitHub App connection for credentials.
///
/// `repo` is `owner/name`. Fetches a repo-scoped clone token from the backend,
/// clones into `dest_parent/<name>`, then scrubs the remote URL back to the
/// tokenless form so the short-TTL token never persists on disk. Refuses to
/// clone into an existing non-empty directory.
#[tauri::command]
pub async fn github_clone_repo(repo: String, dest_parent: String) -> Result<Value, String> {
    let repo = repo.trim().to_string();
    let dest_parent = dest_parent.trim().to_string();

    let Some((owner, name_raw)) = repo.split_once('/') else {
        return Err(format!(
            "Invalid repository '{}': expected 'owner/name'",
            repo
        ));
    };
    let owner = owner.to_string();
    let name = name_raw.trim_end_matches(".git").to_string();
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(format!(
            "Invalid repository '{}': expected 'owner/name'",
            repo
        ));
    }
    if dest_parent.is_empty() {
        return Err("No destination folder selected".to_string());
    }

    let parent = std::path::PathBuf::from(&dest_parent);
    if !parent.is_dir() {
        return Err(format!(
            "Destination folder does not exist: {}",
            parent.display()
        ));
    }
    let dest = parent.join(&name);
    if dest.exists() {
        let non_empty = std::fs::read_dir(&dest)
            .map(|mut e| e.next().is_some())
            .unwrap_or(true);
        if non_empty {
            return Err(format!(
                "A non-empty folder already exists at {}. Remove it or choose a different \
                 destination.",
                dest.display()
            ));
        }
    }

    // 1. Obtain a repo-scoped clone token from the backend.
    let Some(bearer) = cognito_bearer().await else {
        return Err("Sign in to your Qontinui account to clone from GitHub.".to_string());
    };
    let cred_url = format!(
        "{}/api/v1/operations/github/clone-credential",
        crate::api_config::get_api_base_url()
    );
    let client = reqwest::Client::new();
    let resp = client
        .post(&cred_url)
        .bearer_auth(&bearer)
        .json(&serde_json::json!({ "repo": format!("{}/{}", owner, name) }))
        .send()
        .await
        .map_err(|e| format!("Failed to reach Qontinui backend: {}", e))?;
    let status = resp.status();
    let cred: Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse clone credential: {}", e))?;
    if !status.is_success() {
        let msg = cred
            .get("message")
            .or_else(|| cred.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("clone credential request failed");
        return Err(msg.to_string());
    }
    let token = cred
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("Backend did not return a clone token")?
        .to_string();

    info!(
        "Setup wizard: cloning {}/{} into {}",
        owner,
        name,
        dest.display()
    );

    // 2. Clone with the token embedded in the URL, then scrub the remote so the
    //    token never persists in `.git/config`. The token is repo-scoped +
    //    short-TTL, so brief in-URL use is acceptable.
    let clean_url = format!("https://github.com/{}/{}.git", owner, name);
    let auth_url = format!(
        "https://x-access-token:{}@github.com/{}/{}.git",
        token, owner, name
    );
    let dest_str = dest.to_string_lossy().to_string();

    let clone_out = tokio::task::spawn_blocking(move || {
        crate::process_helpers::no_window("git")
            .args(["clone", &auth_url, &dest_str])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    })
    .await
    .map_err(|e| String::from(AppError::ProcessError(format!("Task join error: {}", e))))?
    .map_err(|e| {
        String::from(AppError::ProcessError(format!(
            "Failed to run git: {}. Is git installed?",
            e
        )))
    })?;

    if !clone_out.status.success() {
        let stderr = String::from_utf8_lossy(&clone_out.stderr);
        // Redact the token if git ever echoes the URL into an error message.
        let redacted = stderr.replace(&token, "***");
        warn!("git clone failed: {}", redacted);
        return Err(format!("Clone failed: {}", redacted.trim()));
    }

    // Scrub: repoint origin at the tokenless URL (best-effort — a failure here
    // leaves a working clone, just with the short-TTL token in the remote URL
    // until it expires).
    let dest_scrub = dest.to_string_lossy().to_string();
    let _ = tokio::task::spawn_blocking(move || {
        crate::process_helpers::no_window("git")
            .args(["-C", &dest_scrub, "remote", "set-url", "origin", &clean_url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await;

    info!(
        "Setup wizard: cloned {}/{} -> {}",
        owner,
        name,
        dest.display()
    );
    Ok(serde_json::json!({
        "path": dest.to_string_lossy().to_string(),
        "name": name,
    }))
}

// ============================================================================
// Claude Config Directory Discovery (pure Rust, no Python CLI)
// ============================================================================

/// Auto-discover Claude Code config directories on this machine.
///
/// Scans common locations for directories containing a `projects/` subfolder:
/// - `CLAUDE_CONFIG_DIR` env var
/// - `C:\claude\.claude-*\` (multi-account setups)
/// - `%USERPROFILE%\.claude` (standard location)
/// - `%LOCALAPPDATA%\claude` (alternate install)
#[tauri::command]
pub fn discover_claude_config_dirs() -> Result<Vec<Value>, String> {
    info!("Setup wizard: discovering Claude Code config directories");
    let mut results: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add = |path: std::path::PathBuf, label: &str, source: &str| {
        let path_str = path.to_string_lossy().to_string();
        if path.join("projects").exists() && seen.insert(path_str.clone()) {
            results.push(serde_json::json!({
                "path": path_str,
                "label": label,
                "source": source,
            }));
        }
    };

    // 1. CLAUDE_CONFIG_DIR env var
    if let Ok(env_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let p = std::path::PathBuf::from(&env_dir);
        let label = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| env_dir.clone());
        add(p, &label, "CLAUDE_CONFIG_DIR env var");
    }

    // 2. Scan C:\claude\.claude-*\ (multi-account setups on Windows)
    let mut found_per_account = false;
    let claude_root = std::path::Path::new("C:\\claude");
    if claude_root.is_dir() {
        if let Ok(entries) = std::fs::read_dir(claude_root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name.starts_with(".claude-") {
                            let label = name.to_string();
                            add(path, &label, "auto-detected");
                            found_per_account = true;
                        }
                    }
                }
            }
        }
    }

    // 3. %USERPROFILE%\.claude (standard location)
    // Skip the default dir when per-account dirs exist — it always shares
    // credentials with one of them and would show as a duplicate.
    if !found_per_account {
        if let Ok(home) = std::env::var("USERPROFILE") {
            let home_claude = std::path::PathBuf::from(&home).join(".claude");
            add(home_claude, ".claude", "home directory");
        }
    }

    // 4. %LOCALAPPDATA%\claude (alternate install location)
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let local_claude = std::path::PathBuf::from(&local_app_data).join("claude");
        add(local_claude, "claude (LocalAppData)", "auto-detected");
    }

    info!("Discovered {} Claude config directories", results.len());
    Ok(results)
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
        AiProvider::Ollama => {
            if let Some(m) = model {
                ai_settings.ollama.model = m;
            }
        }
        AiProvider::OpenAiCompatible => {
            if let Some(m) = model {
                ai_settings.openai_compatible.model = m;
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
    // Auto-populate build command based on the run command/framework
    let (build_cmd, build_args_val): (Option<&str>, Vec<&str>) = match command {
        "cargo" => (Some("cargo"), vec!["build"]),
        "npm" | "npx" => (Some("npm"), vec!["run", "build"]),
        "poetry" => (Some("poetry"), vec!["install"]),
        "python" => {
            if parser == "python" {
                (Some("pip"), vec!["install", "-e", "."])
            } else {
                (None, vec![])
            }
        }
        _ => (None, vec![]),
    };

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
        "start_group": 0,
        "dev_only": false,
    });
    if let Some(port) = health_port {
        config["health_port"] = serde_json::json!(port);
    }
    if let Some(bc) = build_cmd {
        config["build_command"] = serde_json::json!(bc);
        config["build_args"] = serde_json::json!(build_args_val);
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

/// Extract the port from a package.json script string.
///
/// Parses common `--port N`, `-p N`, and `--port=N` patterns from npm scripts
/// like `"next dev --port 3001"` or `"vite --port 4000"`.
fn detect_port_from_script(script: &str) -> Option<u16> {
    let parts: Vec<&str> = script.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        // --port=N or -p=N
        if let Some(val) = part
            .strip_prefix("--port=")
            .or_else(|| part.strip_prefix("-p="))
        {
            if let Ok(port) = val.parse::<u16>() {
                return Some(port);
            }
        }
        // --port N or -p N
        if (*part == "--port" || *part == "-p") && i + 1 < parts.len() {
            if let Ok(port) = parts[i + 1].parse::<u16>() {
                return Some(port);
            }
        }
    }
    None
}

/// Read the port from a Node project's dev or start script in package.json.
///
/// Checks the `dev` script first, then `start`. Returns the framework default
/// if no explicit port is found.
fn detect_node_port(path: &str, script_name: &str, default_port: u16) -> u16 {
    let pkg_path = std::path::Path::new(path).join("package.json");
    if let Ok(contents) = std::fs::read_to_string(&pkg_path) {
        if let Ok(pkg) = serde_json::from_str::<Value>(&contents) {
            if let Some(script) = pkg
                .get("scripts")
                .and_then(|s| s.get(script_name))
                .and_then(|v| v.as_str())
            {
                if let Some(port) = detect_port_from_script(script) {
                    return port;
                }
            }
        }
    }
    default_port
}

/// Generate process configs for a detected framework.
fn configs_for_framework(name: &str, path: &str, framework: &str) -> Vec<Value> {
    match framework {
        "nextjs" => {
            let port = detect_node_port(path, "dev", 3000);
            vec![build_process_config(
                name,
                "Next.js",
                "npm",
                &["run", "dev"],
                path,
                Some(port),
                "javascript",
                "frontend",
            )]
        }
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
        "express" => {
            let port = detect_node_port(path, "start", 3000);
            vec![build_process_config(
                name,
                "Express",
                "npm",
                &["start"],
                path,
                Some(port),
                "javascript",
                "backend",
            )]
        }
        "nestjs" => {
            let port = detect_node_port(path, "start:dev", 3000);
            vec![build_process_config(
                name,
                "NestJS",
                "npm",
                &["run", "start:dev"],
                path,
                Some(port),
                "javascript",
                "backend",
            )]
        }
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
        "vite" => {
            let port = detect_node_port(path, "dev", 5173);
            vec![build_process_config(
                name,
                "Vite",
                "npm",
                &["run", "dev"],
                path,
                Some(port),
                "javascript",
                "frontend",
            )]
        }
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
        "docusaurus" => {
            let port = detect_node_port(path, "start", 3000);
            vec![build_process_config(
                name,
                "Docusaurus",
                "npm",
                &["start"],
                path,
                Some(port),
                "javascript",
                "frontend",
            )]
        }
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

// ============================================================================
// Dev Services (Runner-managed services for dev-mode)
// ============================================================================

/// Suggest dev-mode services that the runner should manage.
///
/// Detects the qontinui workspace and returns ProcessConfig-compatible JSON
/// for services like Docker, Backend, and Frontend that should auto-start
/// with the runner in development mode.
///
/// Each suggested config includes `start_group` for ordered startup and
/// `dev_only: true` to ensure they're only active in dev builds.
#[tauri::command]
pub async fn suggest_dev_services_for_setup(
    already_selected: Option<Vec<Value>>,
) -> Result<Vec<Value>, String> {
    info!("Setup wizard: suggesting dev services");

    let workspace = crate::dev_services::find_workspace_root()
        .ok_or_else(|| "Could not find qontinui workspace root".to_string())?;

    let mut existing = settings::get_managed_process_configs();

    // Include process configs already selected in earlier wizard steps so we
    // don't suggest dev services that duplicate them (same health_port).
    // We force auto_start + enabled so port_has_auto_start() treats them as
    // active — the originals default to auto_start=false but the user has
    // explicitly selected them, so they'll be started.
    if let Some(selected) = already_selected {
        for val in selected {
            if let Ok(mut cfg) =
                serde_json::from_value::<crate::process_capture::ProcessConfig>(val)
            {
                cfg.auto_start = true;
                cfg.enabled = true;
                existing.push(cfg);
            }
        }
    }

    let services = crate::dev_services::get_missing_dev_services(&workspace, &existing);

    let result: Vec<Value> = services
        .into_iter()
        .map(|svc| {
            serde_json::json!({
                "id": svc.id,
                "name": svc.name,
                "command": svc.command,
                "args": svc.args,
                "cwd": svc.cwd,
                "env": svc.env,
                "health_port": svc.health_port,
                "parser": svc.parser,
                "category": svc.category,
                "auto_start": svc.auto_start,
                "enabled": svc.enabled,
                "buffer_size": svc.buffer_size,
                "start_group": svc.start_group,
                "dev_only": svc.dev_only,
            })
        })
        .collect();

    info!("Suggested {} dev services", result.len());
    Ok(result)
}

/// Save selected dev services from the setup wizard.
///
/// Saves the selected services as managed process configs with `auto_start: true`
/// and `dev_only: true`. Services not selected are saved with `auto_start: false`.
#[tauri::command]
pub fn save_dev_services_from_setup(
    services: Vec<Value>,
    selected_ids: Vec<String>,
) -> Result<(), String> {
    info!(
        "Setup wizard: saving {} dev services ({} selected)",
        services.len(),
        selected_ids.len()
    );

    for service_value in services {
        let mut config: crate::process_capture::ProcessConfig =
            serde_json::from_value(service_value).map_err(|e| {
                String::from(AppError::ValidationError(format!(
                    "Invalid process config: {}",
                    e
                )))
            })?;

        // Set auto_start based on whether the user selected this service
        config.auto_start = selected_ids.contains(&config.id);
        config.dev_only = true;

        settings::save_managed_process_config(config)?;
    }

    Ok(())
}
