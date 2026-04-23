//! UI Bridge Integration Module
//!
//! Provides project analysis, source code integration,
//! and integration status tracking for the UI Bridge SDK.

use axum::{
    extract::{Path, State},
    response::Json,
    routing::{delete, get, post},
    Router,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use super::types::{ApiResponse, ApiState};

// ============================================================================
// Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Framework {
    React,
    NextJs,
    Vue,
    Angular,
    Svelte,
    PlainHtml,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
    Bun,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerFramework {
    NextJs,
    Express,
    Fastify,
    Axum,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    None,
    Partial,
    Full,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryPoint {
    pub path: String,
    pub entry_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectAnalysis {
    pub project_path: String,
    pub framework: Framework,
    pub package_manager: PackageManager,
    pub entry_points: Vec<EntryPoint>,
    pub ui_bridge_status: IntegrationStatus,
    pub existing_sdk_version: Option<String>,
    pub has_babel_plugin: bool,
    pub has_swc_plugin: bool,
    pub server_framework: ServerFramework,
    pub server_adapter: Option<String>,
    pub server_entry_points: Vec<String>,
    pub dev_server_port: Option<u16>,
    pub has_generated_hooks: bool,
    pub has_architecture_spec: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeRequest {
    pub project_path: String,
}

#[derive(Debug, Deserialize)]
pub struct IntegrateRequest {
    pub project_path: String,
    #[serde(default)]
    pub options: IntegrationOptions,
}

#[derive(Debug, Deserialize, Default)]
pub struct IntegrationOptions {
    #[serde(default = "default_true")]
    pub install_deps: bool,
    /// Deprecated: AutoRegisterProvider replaces build-time babel/swc instrumentation.
    /// This field is accepted but ignored.
    #[serde(default)]
    pub auto_instrument: bool,
    pub sdk_version: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct WriteHooksRequest {
    pub project_path: String,
    pub files: Vec<WriteHooksFileEntry>,
}

#[derive(Debug, Deserialize)]
pub struct WriteHooksFileEntry {
    pub file_path: String,
    pub modification_type: ModificationType,
    pub new_content: String,
}

#[derive(Debug, Serialize)]
pub struct WriteHooksResult {
    pub success: bool,
    pub files_written: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadFileRequest {
    pub project_path: String,
    pub file_path: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRequest {
    pub project_path: String,
    pub sdk_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModificationType {
    Insert,
    Replace,
    CreateNew,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileModification {
    pub file_path: String,
    pub modification_type: ModificationType,
    pub description: String,
    pub original_content: Option<String>,
    pub new_content: String,
}

#[derive(Debug, Serialize)]
pub struct IntegrationResult {
    pub success: bool,
    pub modifications: Vec<FileModification>,
    pub install_output: Option<String>,
    pub warnings: Vec<String>,
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackedIntegration {
    pub id: String,
    pub project_path: String,
    pub label: Option<String>,
    pub framework: Option<String>,
    pub integration_type: String,
    pub sdk_version: Option<String>,
    pub status: String,
    pub target_url: Option<String>,
    pub last_health_check: Option<i64>,
    pub element_count: Option<u32>,
    pub created_at: i64,
    pub updated_at: i64,
}

// ============================================================================
// Project Analyzer
// ============================================================================

async fn analyze_project(path: &str) -> Result<ProjectAnalysis, String> {
    let project_path = PathBuf::from(path);
    if !project_path.exists() {
        return Err(format!("Project path does not exist: {}", path));
    }

    let mut framework = Framework::Unknown;
    let mut package_manager = PackageManager::Unknown;
    let mut entry_points = Vec::new();
    let mut ui_bridge_status = IntegrationStatus::None;
    let mut existing_sdk_version = None;
    let mut has_babel_plugin = false;
    let mut has_swc_plugin = false;
    let mut server_framework = ServerFramework::None;
    let mut server_adapter = None;
    let mut server_entry_points = Vec::new();
    let mut dev_server_port = None;
    let mut issues = Vec::new();

    // Detect package manager from lock files
    if project_path.join("package-lock.json").exists() {
        package_manager = PackageManager::Npm;
    } else if project_path.join("yarn.lock").exists() {
        package_manager = PackageManager::Yarn;
    } else if project_path.join("pnpm-lock.yaml").exists() {
        package_manager = PackageManager::Pnpm;
    } else if project_path.join("bun.lockb").exists() {
        package_manager = PackageManager::Bun;
    }

    // Read package.json for framework detection
    let pkg_json_path = project_path.join("package.json");
    if pkg_json_path.exists() {
        match tokio::fs::read_to_string(&pkg_json_path).await {
            Ok(content) => {
                if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                    let deps = pkg.get("dependencies").cloned().unwrap_or_default();
                    let dev_deps = pkg.get("devDependencies").cloned().unwrap_or_default();

                    // Detect framework from dependencies
                    if deps.get("next").is_some() || dev_deps.get("next").is_some() {
                        framework = Framework::NextJs;
                    } else if deps.get("react").is_some() || dev_deps.get("react").is_some() {
                        framework = Framework::React;
                    } else if deps.get("vue").is_some() || dev_deps.get("vue").is_some() {
                        framework = Framework::Vue;
                    } else if deps.get("@angular/core").is_some()
                        || dev_deps.get("@angular/core").is_some()
                    {
                        framework = Framework::Angular;
                    } else if deps.get("svelte").is_some() || dev_deps.get("svelte").is_some() {
                        framework = Framework::Svelte;
                    }

                    // Check for UI Bridge SDK
                    if let Some(version) = deps
                        .get("@qontinui/ui-bridge")
                        .or_else(|| dev_deps.get("@qontinui/ui-bridge"))
                    {
                        existing_sdk_version = version.as_str().map(|s| s.to_string());
                        ui_bridge_status = IntegrationStatus::Partial;
                    }

                    // Check for babel/swc plugins
                    if deps.get("@qontinui/ui-bridge-babel-plugin").is_some()
                        || dev_deps.get("@qontinui/ui-bridge-babel-plugin").is_some()
                    {
                        has_babel_plugin = true;
                    }
                    if deps.get("@qontinui/ui-bridge-swc-plugin").is_some()
                        || dev_deps.get("@qontinui/ui-bridge-swc-plugin").is_some()
                    {
                        has_swc_plugin = true;
                    }

                    // Detect server framework from dependencies
                    if framework == Framework::NextJs {
                        server_framework = ServerFramework::NextJs;
                    } else if deps.get("express").is_some() || dev_deps.get("express").is_some() {
                        server_framework = ServerFramework::Express;
                    } else if deps.get("fastify").is_some() || dev_deps.get("fastify").is_some() {
                        server_framework = ServerFramework::Fastify;
                    }

                    // Check for server adapter
                    if deps.get("@qontinui/ui-bridge-server").is_some()
                        || dev_deps.get("@qontinui/ui-bridge-server").is_some()
                    {
                        server_adapter = Some(
                            if framework == Framework::NextJs {
                                "nextjs"
                            } else {
                                "standalone"
                            }
                            .to_string(),
                        );
                    }

                    // Detect dev server port from scripts
                    if let Some(scripts) = pkg.get("scripts") {
                        let dev_script = scripts
                            .get("dev")
                            .or_else(|| scripts.get("start"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Some(port) = extract_port_from_script(dev_script) {
                            dev_server_port = Some(port);
                        } else {
                            dev_server_port = match framework {
                                Framework::NextJs => Some(3000),
                                Framework::React => Some(3000),
                                Framework::Vue => Some(5173),
                                Framework::Angular => Some(4200),
                                Framework::Svelte => Some(5173),
                                _ => None,
                            };
                        }
                    }
                }
            }
            Err(e) => issues.push(format!("Failed to read package.json: {}", e)),
        }
    } else if project_path.join("index.html").exists() {
        framework = Framework::PlainHtml;
    }

    // Detect Rust/Axum server framework from Cargo.toml (check project dir and parent)
    if server_framework == ServerFramework::None {
        for cargo_dir in &[project_path.clone(), project_path.join("..").to_path_buf()] {
            let cargo_path = cargo_dir.join("Cargo.toml");
            if cargo_path.exists() {
                if let Ok(cargo_content) = tokio::fs::read_to_string(&cargo_path).await {
                    if cargo_content.contains("axum") {
                        server_framework = ServerFramework::Axum;
                        break;
                    }
                }
            }
        }
    }

    // Detect server entry points
    for entry in &[
        "server.ts",
        "server.js",
        "src/server.ts",
        "src/server.js",
        "src/index.ts",
        "src/index.js",
    ] {
        if project_path.join(entry).exists() {
            server_entry_points.push(entry.to_string());
        }
    }

    // Find entry points based on framework
    match framework {
        Framework::NextJs => {
            for path_str in &[
                "app/layout.tsx",
                "app/layout.jsx",
                "src/app/layout.tsx",
                "src/app/layout.jsx",
            ] {
                if project_path.join(path_str).exists() {
                    entry_points.push(EntryPoint {
                        path: path_str.to_string(),
                        entry_type: "layout".to_string(),
                    });
                }
            }
            for path_str in &["pages/_app.tsx", "pages/_app.jsx", "src/pages/_app.tsx"] {
                if project_path.join(path_str).exists() {
                    entry_points.push(EntryPoint {
                        path: path_str.to_string(),
                        entry_type: "app_root".to_string(),
                    });
                }
            }
            for path_str in &[
                "app/api/ui-bridge",
                "src/app/api/ui-bridge",
                "pages/api/ui-bridge",
            ] {
                if project_path.join(path_str).exists() {
                    server_adapter = Some("nextjs".to_string());
                }
            }
        }
        Framework::React => {
            for path_str in &[
                "src/App.tsx",
                "src/App.jsx",
                "src/main.tsx",
                "src/main.jsx",
                "src/index.tsx",
                "src/index.jsx",
            ] {
                if project_path.join(path_str).exists() {
                    entry_points.push(EntryPoint {
                        path: path_str.to_string(),
                        entry_type: "app_root".to_string(),
                    });
                }
            }
        }
        Framework::Vue => {
            for path_str in &["src/App.vue", "src/main.ts", "src/main.js"] {
                if project_path.join(path_str).exists() {
                    entry_points.push(EntryPoint {
                        path: path_str.to_string(),
                        entry_type: "app_root".to_string(),
                    });
                }
            }
        }
        Framework::PlainHtml => {
            if project_path.join("index.html").exists() {
                entry_points.push(EntryPoint {
                    path: "index.html".to_string(),
                    entry_type: "index_html".to_string(),
                });
            }
        }
        _ => {}
    }

    // Scan source for UIBridgeProvider to verify full integration
    if existing_sdk_version.is_some() {
        let has_provider = scan_for_pattern(
            &project_path,
            &["src"],
            "UIBridgeProvider",
            &["tsx", "jsx", "ts", "js"],
        )
        .await;
        if has_provider {
            ui_bridge_status = IntegrationStatus::Full;
        } else {
            issues.push("SDK installed but UIBridgeProvider not found in source".to_string());
        }
    }

    // Check if generated hooks file exists
    let has_generated_hooks = project_path
        .join("src/lib/ui-bridge/UIBridgeHooks.tsx")
        .exists();

    // Check if architecture spec exists
    let has_architecture_spec =
        scan_for_file_pattern(&project_path, ".architecture.uibridge.json").await;

    Ok(ProjectAnalysis {
        project_path: path.to_string(),
        framework,
        package_manager,
        entry_points,
        ui_bridge_status,
        existing_sdk_version,
        has_babel_plugin,
        has_swc_plugin,
        server_framework,
        server_adapter,
        server_entry_points,
        dev_server_port,
        has_generated_hooks,
        has_architecture_spec,
        issues,
    })
}

fn extract_port_from_script(script: &str) -> Option<u16> {
    let patterns = [
        r"--port[= ](\d+)",
        r"-p[= ](\d+)",
        r"PORT=(\d+)",
        r"port[= ](\d+)",
    ];
    for pattern in &patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(script) {
                if let Some(port_str) = caps.get(1) {
                    return port_str.as_str().parse().ok();
                }
            }
        }
    }
    None
}

async fn scan_for_pattern(
    project_path: &std::path::Path,
    dirs: &[&str],
    pattern: &str,
    extensions: &[&str],
) -> bool {
    for dir in dirs {
        let search_dir = project_path.join(dir);
        if !search_dir.exists() {
            continue;
        }
        if let Ok(mut read_dir) = tokio::fs::read_dir(&search_dir).await {
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                    if dir_name == "node_modules" || dir_name == ".next" || dir_name == "dist" {
                        continue;
                    }
                    let sub_dirs = [dir_name.to_string()];
                    let sub_dir_refs: Vec<&str> = sub_dirs.iter().map(|s| s.as_str()).collect();
                    if Box::pin(scan_for_pattern(
                        &search_dir,
                        &sub_dir_refs,
                        pattern,
                        extensions,
                    ))
                    .await
                    {
                        return true;
                    }
                } else if let Some(ext) = path.extension() {
                    let ext_str = ext.to_string_lossy();
                    if extensions.iter().any(|e| *e == ext_str.as_ref()) {
                        if let Ok(content) = tokio::fs::read_to_string(&path).await {
                            if content.contains(pattern) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Check if any file matching a filename suffix exists in the project root or src/.
async fn scan_for_file_pattern(project_path: &std::path::Path, suffix: &str) -> bool {
    // Check project root
    if let Ok(mut read_dir) = tokio::fs::read_dir(project_path).await {
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(suffix) {
                return true;
            }
        }
    }
    // Check src/ and src/specs/ and src/lib/
    for sub in &["src", "src/specs", "src/lib"] {
        let dir = project_path.join(sub);
        if let Ok(mut read_dir) = tokio::fs::read_dir(&dir).await {
            while let Ok(Some(entry)) = read_dir.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(suffix) {
                    return true;
                }
            }
        }
    }
    false
}

// ============================================================================
// Package Manager Execution
// ============================================================================

/// Run a package install command (npm install, yarn install, etc.) in the project directory.
/// Returns the combined stdout/stderr on success, or an error message on failure.
async fn run_package_install(project_path: &str, command: &str) -> Result<String, String> {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.is_empty() {
        return Err("Empty install command".to_string());
    }

    let program = parts[0];
    let args = &parts[1..];

    let mut cmd = crate::process_helpers::tokio_no_window(program);
    cmd.args(args).current_dir(project_path);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to start '{}': {}. Is {} installed and in PATH?",
            command, e, program
        )
    })?;

    match tokio::time::timeout(
        std::time::Duration::from_secs(300),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let combined = if stderr.is_empty() {
                stdout
            } else {
                format!("{}\n{}", stdout, stderr)
            };

            if output.status.success() {
                Ok(combined)
            } else {
                Err(format!(
                    "'{}' exited with code {}:\n{}",
                    command,
                    output.status.code().unwrap_or(-1),
                    combined.chars().take(2000).collect::<String>()
                ))
            }
        }
        Ok(Err(e)) => Err(format!("Failed to run '{}': {}", command, e)),
        Err(_) => Err(format!("'{}' timed out after 5 minutes", command)),
    }
}

// ============================================================================
// Source Code Integrator
// ============================================================================

/// Shared file-writing utility for applying FileModification sets to disk.
/// Returns (success, warnings).
async fn write_file_modifications(
    project: &std::path::Path,
    modifications: &[FileModification],
) -> (bool, Vec<String>) {
    let mut success = true;
    let mut warnings = Vec::new();

    for modification in modifications {
        let file_path = project.join(&modification.file_path);
        match &modification.modification_type {
            ModificationType::CreateNew => {
                if let Some(parent) = file_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                if let Err(e) = tokio::fs::write(&file_path, &modification.new_content).await {
                    warnings.push(format!(
                        "Failed to create {}: {}",
                        modification.file_path, e
                    ));
                    success = false;
                }
            }
            ModificationType::Insert | ModificationType::Replace => {
                if let Err(e) = tokio::fs::write(&file_path, &modification.new_content).await {
                    warnings.push(format!("Failed to write {}: {}", modification.file_path, e));
                    success = false;
                }
            }
        }
    }

    (success, warnings)
}

async fn integrate_source(
    project_path: &str,
    analysis: &ProjectAnalysis,
    options: &IntegrationOptions,
) -> IntegrationResult {
    let mut modifications = Vec::new();
    let mut warnings = Vec::new();
    let mut next_steps = Vec::new();
    let project = PathBuf::from(project_path);

    match analysis.framework {
        Framework::React => {
            integrate_react(
                &project,
                analysis,
                options,
                &mut modifications,
                &mut warnings,
            )
            .await;
            next_steps.push("Restart your dev server to apply changes".to_string());
            next_steps.push(
                "Enabled features: element auto-registration, render logging, control API, debug tools, idle detection, browser event capture, modal/toast detection, navigation tracking, keyboard shortcut discovery, drag-drop detection, undo/redo awareness".to_string()
            );
            next_steps.push(
                "The CommandRelayListener component handles browser-server communication automatically".to_string()
            );
            next_steps.push(
                "Optional: Add useUIRelationship() for element relationships, useUIComponent() for component-level actions, usePageContext() for route metadata, useKeyboardShortcuts() for shortcut registration".to_string()
            );
            next_steps.push(
                "Optional: Register form library adapters (React Hook Form, Formik) for accurate form state extraction".to_string()
            );
        }
        Framework::NextJs => {
            integrate_nextjs(
                &project,
                analysis,
                options,
                &mut modifications,
                &mut warnings,
            )
            .await;
            next_steps.push("Restart your dev server to apply changes".to_string());
            next_steps.push(
                "Enabled features: element auto-registration, render logging, control API, debug tools, idle detection, browser event capture, modal/toast detection, navigation tracking, keyboard shortcut discovery, drag-drop detection, undo/redo awareness".to_string()
            );
            next_steps.push(
                "The CommandRelayListener component and relay setup handle browser-server communication automatically".to_string()
            );
            next_steps.push(
                "Optional: Add <RenderLogWrapper> inside <AutoRegisterProvider> in your root layout for automatic DOM snapshot capture".to_string()
            );
            next_steps.push(
                "Optional: Add useUIRelationship() for element relationships, useUIComponent() for component-level actions, usePageContext() for route metadata, useKeyboardShortcuts() for shortcut registration".to_string()
            );
            next_steps.push(
                "Optional: Register form library adapters (React Hook Form, Formik) for accurate form state extraction".to_string()
            );
        }
        Framework::Vue | Framework::Angular | Framework::Svelte => {
            warnings.push(format!(
                "{:?} detected. Full SDK integration is currently only available for React/Next.js. Manual integration is required for other frameworks.",
                analysis.framework
            ));
            next_steps.push("Install @qontinui/ui-bridge and integrate manually following the SDK documentation".to_string());
        }
        Framework::PlainHtml => {
            warnings.push("Plain HTML detected. Full SDK integration requires a React or Next.js project. Consider using @qontinui/ui-bridge with a bundler setup.".to_string());
        }
        Framework::Unknown => {
            warnings.push("Could not detect framework. Please integrate manually.".to_string());
        }
    }

    // Server-side relay integration (separate from client-side framework setup).
    // Next.js server relay is already handled in integrate_nextjs().
    match analysis.server_framework {
        ServerFramework::Express => {
            integrate_express_server(&project, analysis, &mut modifications, &mut warnings).await;
            next_steps.push(
                "Server-side: Express relay router created. Mount it in your server with: app.use('/api/ui-bridge', uiBridgeRouter)"
                    .to_string(),
            );
        }
        ServerFramework::Axum => {
            next_steps.push(
                "Server-side: Axum detected. Add UI Bridge relay endpoints to your Axum server. See qontinui-supervisor/src/routes/supervisor_bridge.rs for a reference Rust implementation with SSE command stream, command response handler, and control endpoints."
                    .to_string(),
            );
        }
        ServerFramework::Fastify => {
            next_steps.push(
                "Server-side: Fastify detected. Create relay setup with CommandRelay + createRelayHandlers from @qontinui/ui-bridge/server. Register routes manually following the Express adapter pattern."
                    .to_string(),
            );
        }
        ServerFramework::None => {
            // For React (non-Next.js) apps with no server framework detected,
            // offer the standalone server option.
            if analysis.framework == Framework::React
                && analysis.server_framework == ServerFramework::None
            {
                integrate_standalone_server(&project, &mut modifications, &mut warnings).await;
                next_steps.push(
                    "Server-side: No server framework detected. Created a standalone UI Bridge server file. Run it with: npx tsx ui-bridge-server.ts"
                        .to_string(),
                );
            }
        }
        ServerFramework::NextJs => {
            // Already handled in integrate_nextjs()
        }
    }

    // Apply file modifications first (so package.json is written before install)
    let (success, write_warnings) = write_file_modifications(&project, &modifications).await;
    warnings.extend(write_warnings);

    // Auto-run package install after files are written
    let install_output =
        if success && options.install_deps && analysis.framework != Framework::PlainHtml {
            let pkg_modified = modifications
                .iter()
                .any(|m| m.file_path.ends_with("package.json"));
            if pkg_modified {
                let cmd = match analysis.package_manager {
                    PackageManager::Yarn => "yarn install",
                    PackageManager::Pnpm => "pnpm install",
                    PackageManager::Bun => "bun install",
                    _ => "npm install",
                };
                match run_package_install(project_path, cmd).await {
                    Ok(output) => Some(format!("Successfully ran `{}`:\n{}", cmd, output)),
                    Err(e) => {
                        warnings.push(format!("Auto-install failed: {}", e));
                        next_steps.insert(
                            0,
                            format!("Run `{}` manually in your project directory", cmd),
                        );
                        Some(format!("Auto-install failed. Run `{}` manually.", cmd))
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

    IntegrationResult {
        success,
        modifications,
        install_output,
        warnings,
        next_steps,
    }
}

async fn integrate_react(
    project: &std::path::Path,
    analysis: &ProjectAnalysis,
    options: &IntegrationOptions,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    add_sdk_to_package_json(project, options, modifications, warnings).await;

    // AutoRegisterProvider replaces build-time babel/swc plugins for element discovery.
    // No babel plugin installation needed.

    let root_entry = analysis
        .entry_points
        .iter()
        .find(|e| e.entry_type == "app_root");

    if let Some(entry) = root_entry {
        let entry_path = project.join(&entry.path);
        if let Ok(content) = tokio::fs::read_to_string(&entry_path).await {
            if !content.contains("UIBridgeProvider") {
                let new_content = wrap_with_provider_react(&content, &entry.path);
                modifications.push(FileModification {
                    file_path: entry.path.clone(),
                    modification_type: ModificationType::Replace,
                    description: "Wrap root component with UIBridgeProvider (features: renderLog, control, debug)".to_string(),
                    original_content: Some(content),
                    new_content,
                });
            }
        }
    } else {
        warnings
            .push("No root entry point found. Please add UIBridgeProvider manually.".to_string());
    }
}

async fn integrate_nextjs(
    project: &std::path::Path,
    analysis: &ProjectAnalysis,
    options: &IntegrationOptions,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    add_sdk_to_package_json(project, options, modifications, warnings).await;

    // Server adapter is bundled in @qontinui/ui-bridge (no separate server package needed).
    // AutoRegisterProvider replaces build-time babel/swc plugins for element discovery.

    let layout_entry = analysis
        .entry_points
        .iter()
        .find(|e| e.entry_type == "layout");

    if let Some(entry) = layout_entry {
        let entry_path = project.join(&entry.path);
        if let Ok(content) = tokio::fs::read_to_string(&entry_path).await {
            if !content.contains("UIBridgeProvider") {
                let new_content = wrap_with_provider_nextjs(&content);
                modifications.push(FileModification {
                    file_path: entry.path.clone(),
                    modification_type: ModificationType::Replace,
                    description: "Wrap root layout with UIBridgeProvider (features: renderLog, control, debug)".to_string(),
                    original_content: Some(content),
                    new_content,
                });
            }
        }
    } else {
        warnings.push(
            "No layout.tsx found. Please add UIBridgeProvider to your root layout.".to_string(),
        );
    }

    // Create relay setup file (CommandRelay + handlers instantiation)
    let relay_path = if project.join("src/lib").exists() || project.join("src/app").exists() {
        "src/lib/ui-bridge.ts"
    } else if project.join("lib").exists() {
        "lib/ui-bridge.ts"
    } else {
        "src/lib/ui-bridge.ts"
    };

    if !project.join(relay_path).exists() {
        modifications.push(FileModification {
            file_path: relay_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create CommandRelay + handlers setup for UI Bridge".to_string(),
            original_content: None,
            new_content: NEXTJS_RELAY_SETUP_TEMPLATE.to_string(),
        });
    }

    // Create API route for UI Bridge endpoints
    let api_route_path = if project.join("src/app").exists() {
        "src/app/api/ui-bridge/[...path]/route.ts"
    } else {
        "app/api/ui-bridge/[...path]/route.ts"
    };

    if !project.join(api_route_path).exists() {
        modifications.push(FileModification {
            file_path: api_route_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create Next.js API route for UI Bridge with command relay".to_string(),
            original_content: None,
            new_content: NEXTJS_API_ROUTE_TEMPLATE.to_string(),
        });
    }

    // Create RenderLogWrapper component for DOM snapshot capture on navigation
    let wrapper_path = if project.join("src/lib").exists() || project.join("src/app").exists() {
        "src/lib/ui-bridge/RenderLogWrapper.tsx"
    } else if project.join("lib").exists() {
        "lib/ui-bridge/RenderLogWrapper.tsx"
    } else {
        "src/lib/ui-bridge/RenderLogWrapper.tsx"
    };

    if !project.join(wrapper_path).exists() {
        modifications.push(FileModification {
            file_path: wrapper_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description:
                "Create RenderLogWrapper for DOM snapshot capture on navigation and mutations"
                    .to_string(),
            original_content: None,
            new_content: NEXTJS_RENDER_LOG_WRAPPER_TEMPLATE.to_string(),
        });
        warnings.push(format!(
            "Created {}. Add <RenderLogWrapper> inside <AutoRegisterProvider> in your root layout for automatic DOM snapshot capture on route changes.",
            wrapper_path
        ));
    }
}

async fn add_sdk_to_package_json(
    project: &std::path::Path,
    options: &IntegrationOptions,
    modifications: &mut Vec<FileModification>,
    _warnings: &mut Vec<String>,
) {
    let pkg_path = project.join("package.json");
    if let Ok(content) = tokio::fs::read_to_string(&pkg_path).await {
        if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            let version = options.sdk_version.as_deref().unwrap_or("latest");
            let deps = pkg.get_mut("dependencies").and_then(|d| d.as_object_mut());
            if let Some(deps) = deps {
                if !deps.contains_key("@qontinui/ui-bridge") {
                    deps.insert(
                        "@qontinui/ui-bridge".to_string(),
                        serde_json::Value::String(version.to_string()),
                    );
                    let new_content =
                        serde_json::to_string_pretty(&pkg).unwrap_or_else(|_| content.clone());
                    modifications.push(FileModification {
                        file_path: "package.json".to_string(),
                        modification_type: ModificationType::Replace,
                        description: "Add @qontinui/ui-bridge to dependencies".to_string(),
                        original_content: Some(content),
                        new_content,
                    });
                }
            }
        }
    }
}

// Server adapter is now bundled in @qontinui/ui-bridge (no separate @qontinui/ui-bridge-server needed).
// Babel/SWC plugins are deprecated — AutoRegisterProvider handles element discovery at runtime.

fn wrap_with_provider_react(content: &str, file_path: &str) -> String {
    let import_line =
        "import { UIBridgeProvider, AutoRegisterProvider, CommandRelayListener } from '@qontinui/ui-bridge/react';\n";
    let insert_pos = find_last_import_pos(content);

    let provider_open = "<UIBridgeProvider\n          features={{ renderLog: true, control: true, debug: process.env.NODE_ENV === 'development' }}\n        >";
    let provider_close = "</UIBridgeProvider>";

    let is_main = file_path.contains("main.") || file_path.contains("index.");
    if is_main {
        let mut result = String::new();
        result.push_str(&content[..insert_pos]);
        result.push_str(import_line);
        let rest = &content[insert_pos..];
        let wrapped = rest
            .replace(
                "<App />",
                &format!("{}\n          <AutoRegisterProvider>\n            <CommandRelayListener />\n            <App />\n          </AutoRegisterProvider>\n        {}", provider_open, provider_close),
            )
            .replace(
                "<App/>",
                &format!("{}\n          <AutoRegisterProvider>\n            <CommandRelayListener />\n            <App />\n          </AutoRegisterProvider>\n        {}", provider_open, provider_close),
            );
        result.push_str(&wrapped);
        result
    } else {
        let mut result = String::new();
        result.push_str(&content[..insert_pos]);
        result.push_str(import_line);
        result.push_str(&content[insert_pos..]);
        result
    }
}

fn wrap_with_provider_nextjs(content: &str) -> String {
    let import_line =
        "import { UIBridgeProvider, AutoRegisterProvider, CommandRelayListener } from '@qontinui/ui-bridge/react';\n";
    let insert_pos = find_last_import_pos(content);

    let mut result = String::new();
    result.push_str(&content[..insert_pos]);
    result.push_str(import_line);
    let rest = &content[insert_pos..];
    let wrapped = rest.replace(
        "{children}",
        "{/* UI Bridge Integration */}\n          <UIBridgeProvider\n            features={{ renderLog: true, control: true, debug: process.env.NODE_ENV === 'development' }}\n          >\n            <AutoRegisterProvider>\n              <CommandRelayListener />\n              {children}\n            </AutoRegisterProvider>\n          </UIBridgeProvider>",
    );
    result.push_str(&wrapped);
    result
}

/// For projects that already have UIBridgeProvider but are missing CommandRelayListener,
/// add the import and component. This handles the "upgrade" case where a project was
/// integrated before CommandRelayListener existed.
async fn add_missing_command_relay_listener(
    project: &std::path::Path,
    analysis: &ProjectAnalysis,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    // Find the layout/entry file
    let entry = match analysis.framework {
        Framework::NextJs => analysis
            .entry_points
            .iter()
            .find(|e| e.entry_type == "layout"),
        Framework::React => analysis
            .entry_points
            .iter()
            .find(|e| e.entry_type == "app_root"),
        _ => None,
    };

    let entry = match entry {
        Some(e) => e,
        None => return,
    };

    let entry_path = project.join(&entry.path);
    let content = match tokio::fs::read_to_string(&entry_path).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Only proceed if UIBridgeProvider is present but CommandRelayListener is not
    if !content.contains("UIBridgeProvider") || content.contains("CommandRelayListener") {
        return;
    }

    // Check if this file is already being modified by another step (avoid double-modify)
    if modifications.iter().any(|m| m.file_path == entry.path) {
        return;
    }

    // Add CommandRelayListener import and component
    let mut new_content = content.clone();

    // Add CommandRelayListener to existing import if possible
    if new_content.contains("import { UIBridgeProvider")
        && !new_content.contains("CommandRelayListener")
    {
        // Try to add to existing import line
        new_content = new_content.replace(
            "} from '@qontinui/ui-bridge/react'",
            ", CommandRelayListener } from '@qontinui/ui-bridge/react'",
        );
        // Also handle double-quotes
        new_content = new_content.replace(
            "} from \"@qontinui/ui-bridge/react\"",
            ", CommandRelayListener } from \"@qontinui/ui-bridge/react\"",
        );
    }

    // Add <CommandRelayListener /> inside the provider tree
    // Try common patterns: after <AutoRegisterProvider>, or after <UIBridgeProvider...>
    if new_content.contains("<AutoRegisterProvider>")
        && !new_content.contains("<CommandRelayListener")
    {
        new_content = new_content.replace(
            "<AutoRegisterProvider>",
            "<AutoRegisterProvider>\n              <CommandRelayListener />",
        );
    } else if new_content.contains("<UIBridgeProvider")
        && !new_content.contains("<CommandRelayListener")
    {
        // Find the closing > of UIBridgeProvider and add after it
        if let Some(pos) = new_content.find("<UIBridgeProvider") {
            if let Some(close) = new_content[pos..].find('>') {
                let insert_at = pos + close + 1;
                new_content.insert_str(insert_at, "\n              <CommandRelayListener />");
            }
        }
    }

    if new_content != content {
        modifications.push(FileModification {
            file_path: entry.path.clone(),
            modification_type: ModificationType::Replace,
            description: "Add CommandRelayListener for relay-based command handling".to_string(),
            original_content: Some(content),
            new_content,
        });
        warnings.push("Added CommandRelayListener to your layout — this enables relay-based command handling between the server and browser.".to_string());
    }
}

/// Generate server-side relay files for Express apps.
async fn integrate_express_server(
    project: &std::path::Path,
    _analysis: &ProjectAnalysis,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    // Create relay setup file (shared pattern with Next.js)
    let relay_path = if project.join("src/lib").exists() {
        "src/lib/ui-bridge.ts"
    } else {
        "src/lib/ui-bridge.ts"
    };

    if !project.join(relay_path).exists() {
        if let Some(parent) = project.join(relay_path).parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        modifications.push(FileModification {
            file_path: relay_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create CommandRelay + handlers setup for UI Bridge".to_string(),
            original_content: None,
            new_content: RELAY_SETUP_TEMPLATE.to_string(),
        });
    }

    // Create Express router file
    let routes_path = "src/ui-bridge-routes.ts";
    if !project.join(routes_path).exists() {
        modifications.push(FileModification {
            file_path: routes_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create Express router for UI Bridge endpoints".to_string(),
            original_content: None,
            new_content: EXPRESS_ROUTES_TEMPLATE.to_string(),
        });
    }

    warnings.push(
        "Server-side relay files created. Add the router to your Express app: import { uiBridgeRouter } from './ui-bridge-routes'; app.use('/api/ui-bridge', uiBridgeRouter);"
            .to_string(),
    );
}

/// Generate a standalone server file for apps without a Node.js server framework.
async fn integrate_standalone_server(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    let server_path = "ui-bridge-server.ts";
    if !project.join(server_path).exists() {
        modifications.push(FileModification {
            file_path: server_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create standalone UI Bridge server for command relay".to_string(),
            original_content: None,
            new_content: STANDALONE_SERVER_TEMPLATE.to_string(),
        });
    }

    warnings.push(
        "Created ui-bridge-server.ts — run it alongside your app to enable UI Bridge relay: npx tsx ui-bridge-server.ts"
            .to_string(),
    );
}

fn find_last_import_pos(content: &str) -> usize {
    let mut last_import_end = 0;
    for (i, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("import ") || trimmed.starts_with("import{") {
            let line_start: usize = content.lines().take(i).map(|l| l.len() + 1).sum();
            let remaining = &content[line_start..];
            if let Some(end) = remaining.find(';') {
                last_import_end = line_start + end + 1;
                if content.as_bytes().get(last_import_end) == Some(&b'\n') {
                    last_import_end += 1;
                }
            }
        }
    }
    last_import_end
}

const NEXTJS_API_ROUTE_TEMPLATE: &str = r#"import { createNextRouteHandlers, SSEManager } from '@qontinui/ui-bridge/server';
import { handlers, relay } from '@/lib/ui-bridge';

const sseManager = new SSEManager();

export const { GET, POST, PUT, DELETE } = createNextRouteHandlers(handlers, {
  relay,
  sseManager,
});

export const dynamic = 'force-dynamic';
export const runtime = 'nodejs';
"#;

const NEXTJS_RELAY_SETUP_TEMPLATE: &str = r#"import { CommandRelay, createRelayHandlers } from '@qontinui/ui-bridge/server';

export const relay = new CommandRelay();
export const handlers = createRelayHandlers(relay);
"#;

const NEXTJS_RENDER_LOG_WRAPPER_TEMPLATE: &str = r#"'use client';

import { useEffect, useRef, useCallback, type ReactNode } from 'react';
import { usePathname, useSearchParams } from 'next/navigation';
import { useUIBridgeOptional } from '@qontinui/ui-bridge/react';

/**
 * RenderLogWrapper — captures DOM snapshots on navigation and significant mutations.
 *
 * Place inside <AutoRegisterProvider> in your root layout:
 *
 *   <UIBridgeProvider features={{ renderLog: true, control: true }}>
 *     <AutoRegisterProvider>
 *       <RenderLogWrapper>{children}</RenderLogWrapper>
 *     </AutoRegisterProvider>
 *   </UIBridgeProvider>
 */
export function RenderLogWrapper({
  children,
  enableOnMount = true,
  enableMutationObserver = true,
  mutationDebounceMs = 500,
}: {
  children: ReactNode;
  enableOnMount?: boolean;
  enableMutationObserver?: boolean;
  mutationDebounceMs?: number;
}) {
  const pathname = usePathname();
  const searchParams = useSearchParams();
  const bridge = useUIBridgeOptional();
  const isDev = process.env.NODE_ENV === 'development';

  const lastPathRef = useRef<string | null>(null);
  const mutationTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const fullPath = pathname + (searchParams?.toString() ? `?${searchParams.toString()}` : '');

  const captureSnapshot = useCallback(
    async (trigger: string, metadata?: Record<string, unknown>) => {
      if (!isDev || !bridge?.renderLog) return;
      await new Promise((resolve) => requestAnimationFrame(resolve));
      await bridge.renderLog.captureSnapshot({ trigger, pathname, ...metadata });
    },
    [isDev, bridge, pathname]
  );

  // Capture on route change
  useEffect(() => {
    if (!isDev || !bridge?.renderLog) return;
    if (lastPathRef.current === fullPath) return;

    const previousPath = lastPathRef.current;
    lastPathRef.current = fullPath;
    if (previousPath === null && enableOnMount) return;

    const timeoutId = setTimeout(() => {
      captureSnapshot('route_change', { previousPath, newPath: fullPath });
    }, 100);
    return () => clearTimeout(timeoutId);
  }, [fullPath, isDev, bridge, captureSnapshot, enableOnMount]);

  // Capture on mount
  useEffect(() => {
    if (!isDev || !bridge?.renderLog || !enableOnMount) return;
    const timeoutId = setTimeout(() => {
      captureSnapshot('mount');
      lastPathRef.current = fullPath;
    }, 500);
    return () => clearTimeout(timeoutId);
  }, [isDev, bridge]);

  // Mutation observer for significant DOM changes
  useEffect(() => {
    if (!isDev || !bridge?.renderLog || !enableMutationObserver) return;

    const observer = new MutationObserver((mutations) => {
      const significant = mutations.some((m) => {
        if (m.addedNodes.length || m.removedNodes.length) {
          for (const node of m.addedNodes) {
            if (node.nodeType === Node.ELEMENT_NODE) {
              const el = node as Element;
              if (!['SCRIPT', 'STYLE', 'SVG'].includes(el.tagName)) return true;
            }
          }
        }
        return false;
      });

      if (significant) {
        if (mutationTimeoutRef.current) clearTimeout(mutationTimeoutRef.current);
        mutationTimeoutRef.current = setTimeout(() => {
          captureSnapshot('mutation');
        }, mutationDebounceMs);
      }
    });

    observer.observe(document.body, { childList: true, subtree: true });

    return () => {
      observer.disconnect();
      if (mutationTimeoutRef.current) clearTimeout(mutationTimeoutRef.current);
    };
  }, [isDev, bridge, enableMutationObserver, mutationDebounceMs, captureSnapshot]);

  return <>{children}</>;
}
"#;

// The relay setup template is shared between Express, Standalone, and any
// Node.js server that uses the CommandRelay pattern.
const RELAY_SETUP_TEMPLATE: &str = r#"import { CommandRelay, createRelayHandlers } from '@qontinui/ui-bridge/server';

export const relay = new CommandRelay();
export const handlers = createRelayHandlers(relay);
"#;

const EXPRESS_ROUTES_TEMPLATE: &str = r#"import { createExpressRouter, SSEManager } from '@qontinui/ui-bridge/server';
import { handlers, relay } from './lib/ui-bridge';

const sseManager = new SSEManager();

/**
 * Express router for UI Bridge endpoints.
 * Mount this in your Express app:
 *
 *   import { uiBridgeRouter } from './ui-bridge-routes';
 *   app.use('/api/ui-bridge', uiBridgeRouter);
 */
export const uiBridgeRouter = createExpressRouter(handlers, {
  relay,
  sseManager,
  cors: true,
});
"#;

const STANDALONE_SERVER_TEMPLATE: &str = r#"import { StandaloneServer, CommandRelay, createRelayHandlers } from '@qontinui/ui-bridge/server';

const relay = new CommandRelay();
const handlers = createRelayHandlers(relay);

const server = new StandaloneServer(handlers, {
  port: 4200,
  cors: true,
});

server.start().then(() => {
  console.log('UI Bridge server running on http://localhost:4200');
});
"#;

// ============================================================================
// Integration Status Tracker (SQLite)
// ============================================================================

fn save_integration(
    _db: &crate::database::pg::PgDb,
    _integration: &TrackedIntegration,
) -> Result<(), String> {
    // checkpoint_db removed — integration persistence not yet migrated to PG
    Ok(())
}

fn list_integrations(_db: &crate::database::pg::PgDb) -> Result<Vec<TrackedIntegration>, String> {
    // checkpoint_db removed — integration list not yet migrated to PG
    Ok(vec![])
}

fn delete_integration(_db: &crate::database::pg::PgDb, _id: &str) -> Result<(), String> {
    // checkpoint_db removed — integration delete not yet migrated to PG
    Ok(())
}

// ============================================================================
// HTTP Handlers
// ============================================================================

async fn handle_analyze(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<AnalyzeRequest>,
) -> Json<ApiResponse<ProjectAnalysis>> {
    let _ = state;
    match analyze_project(&req.project_path).await {
        Ok(analysis) => Json(ApiResponse::success(analysis)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_integrate(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<IntegrateRequest>,
) -> Json<ApiResponse<IntegrationResult>> {
    match analyze_project(&req.project_path).await {
        Ok(analysis) => {
            let result = integrate_source(&req.project_path, &analysis, &req.options).await;

            if result.success {
                let now = chrono::Utc::now().timestamp();
                let integration = TrackedIntegration {
                    id: format!(
                        "source-{}",
                        req.project_path.replace(['/', '\\', ':', ' '], "-")
                    ),
                    project_path: req.project_path,
                    label: None,
                    framework: Some(format!("{:?}", analysis.framework).to_lowercase()),
                    integration_type: "source".to_string(),
                    sdk_version: req.options.sdk_version.or(Some("latest".to_string())),
                    status: "active".to_string(),
                    target_url: analysis
                        .dev_server_port
                        .map(|p| format!("http://localhost:{}", p)),
                    last_health_check: Some(now),
                    element_count: None,
                    created_at: now,
                    updated_at: now,
                };
                let _ = save_integration(&state.app_state.pg_db, &integration);
            }

            Json(ApiResponse::success(result))
        }
        Err(e) => Json(ApiResponse::error(format!(
            "Project analysis failed: {}",
            e
        ))),
    }
}

async fn handle_preview(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<IntegrateRequest>,
) -> Json<ApiResponse<Vec<FileModification>>> {
    match analyze_project(&req.project_path).await {
        Ok(analysis) => {
            let mut modifications = Vec::new();
            let mut warnings = Vec::new();
            let project = PathBuf::from(&req.project_path);

            match analysis.framework {
                Framework::React => {
                    integrate_react(
                        &project,
                        &analysis,
                        &req.options,
                        &mut modifications,
                        &mut warnings,
                    )
                    .await;
                }
                Framework::NextJs => {
                    integrate_nextjs(
                        &project,
                        &analysis,
                        &req.options,
                        &mut modifications,
                        &mut warnings,
                    )
                    .await;
                }
                _ => {}
            }

            // Include server-side integration files in preview
            match analysis.server_framework {
                ServerFramework::Express => {
                    integrate_express_server(
                        &project,
                        &analysis,
                        &mut modifications,
                        &mut warnings,
                    )
                    .await;
                }
                ServerFramework::None if analysis.framework == Framework::React => {
                    integrate_standalone_server(&project, &mut modifications, &mut warnings).await;
                }
                _ => {}
            }

            Json(ApiResponse::success(modifications))
        }
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_status(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<TrackedIntegration>>> {
    match list_integrations(&state.app_state.pg_db) {
        Ok(integrations) => Json(ApiResponse::success(integrations)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_delete_integration(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<String>> {
    match delete_integration(&state.app_state.pg_db, &id) {
        Ok(_) => Json(ApiResponse::success(format!("Integration {} deleted", id))),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_health_check(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<TrackedIntegration>>> {
    let integrations = match list_integrations(&state.app_state.pg_db) {
        Ok(integrations) => integrations,
        Err(e) => return Json(ApiResponse::error(e)),
    };

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    let now = chrono::Utc::now().timestamp();

    let mut updated = Vec::new();
    for mut integration in integrations {
        // Determine the URL to check (integration_type is always "source")
        let check_url = integration
            .target_url
            .as_ref()
            .map(|u| format!("{}/api/ui-bridge/health", u.trim_end_matches('/')));

        if let Some(url) = check_url {
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    integration.status = "active".to_string();
                    integration.last_health_check = Some(now);
                    // Try to get element count from health response
                    if let Ok(body) = resp.json::<serde_json::Value>().await {
                        if let Some(count) = body.get("element_count").and_then(|v| v.as_u64()) {
                            integration.element_count = Some(count as u32);
                        }
                    }
                }
                _ => {
                    // Only mark disconnected if it was previously active
                    if integration.status == "active" {
                        integration.status = "disconnected".to_string();
                    }
                    integration.last_health_check = Some(now);
                }
            }
        }

        integration.updated_at = now;
        let _ = save_integration(&state.app_state.pg_db, &integration);
        updated.push(integration);
    }

    Json(ApiResponse::success(updated))
}

async fn handle_update(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<UpdateRequest>,
) -> Json<ApiResponse<IntegrationResult>> {
    // Analyze the project to get current state
    let analysis = match analyze_project(&req.project_path).await {
        Ok(a) => a,
        Err(e) => {
            return Json(ApiResponse::error(format!(
                "Project analysis failed: {}",
                e
            )))
        }
    };

    // Only update projects that already have the SDK
    if analysis.ui_bridge_status == IntegrationStatus::None {
        return Json(ApiResponse::error(
            "Project does not have UI Bridge SDK installed. Use 'integrate' instead.".to_string(),
        ));
    }

    let new_version = req.sdk_version.as_deref().unwrap_or("latest");

    // Validate version string (must be "latest", semver-like, or npm range)
    if new_version != "latest" && new_version != "next" {
        let version_re = regex::Regex::new(r"^[\^~>=<*]?\d").unwrap();
        if !version_re.is_match(new_version) {
            return Json(ApiResponse::error(format!(
                "Invalid SDK version '{}'. Use 'latest', a semver version (e.g., '1.2.3'), or a range (e.g., '^1.0.0').",
                new_version
            )));
        }
    }

    // Update version in package.json
    let project = PathBuf::from(&req.project_path);
    let pkg_path = project.join("package.json");
    let mut modifications = Vec::new();
    let mut warnings = Vec::new();

    if let Ok(content) = tokio::fs::read_to_string(&pkg_path).await {
        if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            let mut changed = false;

            // Update SDK version in dependencies
            if let Some(deps) = pkg.get_mut("dependencies").and_then(|d| d.as_object_mut()) {
                if deps.contains_key("@qontinui/ui-bridge") {
                    deps.insert(
                        "@qontinui/ui-bridge".to_string(),
                        serde_json::Value::String(new_version.to_string()),
                    );
                    changed = true;
                }
                if deps.contains_key("@qontinui/ui-bridge-server") {
                    deps.insert(
                        "@qontinui/ui-bridge-server".to_string(),
                        serde_json::Value::String(new_version.to_string()),
                    );
                    changed = true;
                }
            }

            // Update babel plugin version in devDependencies
            if let Some(dev_deps) = pkg
                .get_mut("devDependencies")
                .and_then(|d| d.as_object_mut())
            {
                if dev_deps.contains_key("@qontinui/ui-bridge-babel-plugin") {
                    dev_deps.insert(
                        "@qontinui/ui-bridge-babel-plugin".to_string(),
                        serde_json::Value::String(new_version.to_string()),
                    );
                    changed = true;
                }
            }

            if changed {
                let new_content =
                    serde_json::to_string_pretty(&pkg).unwrap_or_else(|_| content.clone());
                modifications.push(FileModification {
                    file_path: "package.json".to_string(),
                    modification_type: ModificationType::Replace,
                    description: format!("Update UI Bridge SDK version to {}", new_version),
                    original_content: Some(content),
                    new_content,
                });
            }
        }
    }

    // Also add any missing features (relay setup, CommandRelayListener, etc.)
    // This allows updating older integrations to include new SDK features.
    let options = IntegrationOptions {
        install_deps: false, // We handle install separately below
        auto_instrument: false,
        sdk_version: Some(new_version.to_string()),
    };
    match analysis.framework {
        Framework::NextJs => {
            integrate_nextjs(
                &project,
                &analysis,
                &options,
                &mut modifications,
                &mut warnings,
            )
            .await;
            // Also check if layout needs CommandRelayListener added
            add_missing_command_relay_listener(
                &project,
                &analysis,
                &mut modifications,
                &mut warnings,
            )
            .await;
        }
        Framework::React => {
            integrate_react(
                &project,
                &analysis,
                &options,
                &mut modifications,
                &mut warnings,
            )
            .await;
            add_missing_command_relay_listener(
                &project,
                &analysis,
                &mut modifications,
                &mut warnings,
            )
            .await;
        }
        _ => {}
    }

    // Apply modifications
    let mut success = true;
    for modification in &modifications {
        let file_path = project.join(&modification.file_path);
        // Ensure parent directory exists for new files
        if let Some(parent) = file_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if let Err(e) = tokio::fs::write(&file_path, &modification.new_content).await {
            warnings.push(format!("Failed to write {}: {}", modification.file_path, e));
            success = false;
        }
    }

    // Update tracker
    if success {
        let integration_id = format!(
            "source-{}",
            req.project_path.replace(['/', '\\', ':', ' '], "-")
        );
        // Update just the version and timestamp in the existing record (PG)
        let pg = state.app_state.pg_db.clone();
        let iid = integration_id.clone();
        let ver = new_version.to_string();
        let _ = pg.update_ui_bridge_integration_version(&iid, &ver).await;
    }

    // Auto-run package install after updating package.json
    let install_cmd = match analysis.package_manager {
        PackageManager::Yarn => "yarn install",
        PackageManager::Pnpm => "pnpm install",
        PackageManager::Bun => "bun install",
        _ => "npm install",
    };

    let mut next_steps = vec![];
    if modifications
        .iter()
        .any(|m| m.modification_type == ModificationType::CreateNew)
    {
        next_steps.push(
            "New files were added — review them to ensure they match your project structure."
                .to_string(),
        );
    }
    next_steps.push("Restart your dev server".to_string());

    let pkg_modified = modifications
        .iter()
        .any(|m| m.file_path.ends_with("package.json"));

    let install_output = if success && pkg_modified {
        match run_package_install(&req.project_path, install_cmd).await {
            Ok(output) => Some(format!("Successfully ran `{}`:\n{}", install_cmd, output)),
            Err(e) => {
                warnings.push(format!("Auto-install failed: {}", e));
                next_steps.insert(
                    0,
                    format!("Run `{}` manually in your project directory", install_cmd),
                );
                Some(format!(
                    "Auto-install failed. Run `{}` manually.",
                    install_cmd
                ))
            }
        }
    } else if pkg_modified {
        next_steps.insert(
            0,
            format!("Run `{}` in your project directory", install_cmd),
        );
        Some(format!("Run `{}` to install updated packages", install_cmd))
    } else {
        None
    };

    Json(ApiResponse::success(IntegrationResult {
        success,
        modifications,
        install_output,
        warnings,
        next_steps,
    }))
}

// ============================================================================
// Write Hooks Handler
// ============================================================================

async fn handle_write_hooks(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<WriteHooksRequest>,
) -> Json<ApiResponse<WriteHooksResult>> {
    let project = PathBuf::from(&req.project_path);
    if !project.exists() {
        return Json(ApiResponse::error(format!(
            "Project path does not exist: {}",
            req.project_path
        )));
    }

    // Convert WriteHooksFileEntry to FileModification for the shared helper
    let modifications: Vec<FileModification> = req
        .files
        .iter()
        .map(|f| FileModification {
            file_path: f.file_path.clone(),
            modification_type: f.modification_type.clone(),
            description: format!("Generated UI Bridge hook file: {}", f.file_path),
            original_content: None,
            new_content: f.new_content.clone(),
        })
        .collect();

    let (success, warnings) = write_file_modifications(&project, &modifications).await;

    let files_written: Vec<String> = if success {
        modifications.iter().map(|m| m.file_path.clone()).collect()
    } else {
        // Only include files that weren't warned about
        modifications
            .iter()
            .filter(|m| !warnings.iter().any(|w| w.contains(&m.file_path)))
            .map(|m| m.file_path.clone())
            .collect()
    };

    // Post-apply: modify source files to integrate generated artifacts
    let mut all_warnings = warnings;
    if success {
        if let Ok(analysis) = analyze_project(&req.project_path).await {
            // Add UIBridgeHooks import to root layout
            let layout_warnings = add_hooks_to_layout(&project, &analysis, &files_written).await;
            all_warnings.extend(layout_warnings);
        }

        // Add registration hook imports to page components
        let reg_warnings = add_registration_imports(&project, &files_written).await;
        all_warnings.extend(reg_warnings);

        // Update tutorial registry with new tutorial files
        let tut_warnings = update_tutorial_registry(&project, &files_written).await;
        all_warnings.extend(tut_warnings);
    }

    Json(ApiResponse::success(WriteHooksResult {
        success,
        files_written,
        warnings: all_warnings,
    }))
}

/// After writing hook files, modify the root layout to import and render <UIBridgeHooks />.
/// Follows the same pattern as `add_missing_command_relay_listener`.
async fn add_hooks_to_layout(
    project: &std::path::Path,
    analysis: &ProjectAnalysis,
    written_files: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Only modify layout if UIBridgeHooks.tsx was written
    let hooks_written = written_files.iter().any(|f| f.contains("UIBridgeHooks"));
    if !hooks_written {
        return warnings;
    }

    // Find the layout/entry file
    let entry = match analysis.framework {
        Framework::NextJs => analysis
            .entry_points
            .iter()
            .find(|e| e.entry_type == "layout"),
        Framework::React => analysis
            .entry_points
            .iter()
            .find(|e| e.entry_type == "app_root"),
        _ => None,
    };

    let entry = match entry {
        Some(e) => e,
        None => return warnings,
    };

    let entry_path = project.join(&entry.path);
    let content = match tokio::fs::read_to_string(&entry_path).await {
        Ok(c) => c,
        Err(_) => return warnings,
    };

    // Skip if UIBridgeHooks is already imported
    if content.contains("UIBridgeHooks") {
        return warnings;
    }

    // Skip if UIBridgeProvider is not present (hooks need to be inside the provider)
    if !content.contains("UIBridgeProvider") {
        return warnings;
    }

    let mut new_content = content.clone();

    // Add import for UIBridgeHooks
    let import_line = "import { UIBridgeHooks } from '@/lib/ui-bridge/UIBridgeHooks';\n";
    let import_pos = find_last_import_pos(&new_content);
    new_content.insert_str(import_pos, import_line);

    // Add <UIBridgeHooks /> inside the provider tree
    // Try common patterns: after <AutoRegisterProvider>, after <CommandRelayListener />,
    // or after <UIBridgeProvider...>
    if new_content.contains("<CommandRelayListener") && !new_content.contains("<UIBridgeHooks") {
        new_content = new_content.replace(
            "<CommandRelayListener />",
            "<CommandRelayListener />\n              <UIBridgeHooks />",
        );
    } else if new_content.contains("<AutoRegisterProvider>")
        && !new_content.contains("<UIBridgeHooks")
    {
        new_content = new_content.replace(
            "<AutoRegisterProvider>",
            "<AutoRegisterProvider>\n              <UIBridgeHooks />",
        );
    } else if new_content.contains("<UIBridgeProvider") && !new_content.contains("<UIBridgeHooks") {
        if let Some(pos) = new_content.find("<UIBridgeProvider") {
            if let Some(close) = new_content[pos..].find('>') {
                let insert_at = pos + close + 1;
                new_content.insert_str(insert_at, "\n              <UIBridgeHooks />");
            }
        }
    }

    if new_content != content {
        if let Err(e) = tokio::fs::write(&entry_path, &new_content).await {
            warnings.push(format!(
                "Failed to add UIBridgeHooks to layout {}: {}",
                entry.path, e
            ));
        } else {
            warnings.push(format!(
                "Added UIBridgeHooks import and component to {}",
                entry.path
            ));
        }
    }

    warnings
}

// ============================================================================
// Post-Apply: Auto-add registration imports to page components
// ============================================================================

/// After writing registration files, modify each page component to import and call
/// the registration hook. Only modifies files that don't already import the hook.
async fn add_registration_imports(
    project: &std::path::Path,
    written_files: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Find registration files among written files
    let reg_files: Vec<&str> = written_files
        .iter()
        .filter(|f| f.contains("-registrations.") && f.contains("/pages/"))
        .map(|f| f.as_str())
        .collect();

    for reg_file in reg_files {
        // Derive the page component path from the registration file name
        // e.g., "src/lib/ui-bridge/pages/dashboard-registrations.tsx" → need to find the page component
        let file_stem = std::path::Path::new(reg_file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let page_name = file_stem.trim_end_matches("-registrations");

        // Derive the hook name: "useDashboardRegistrations"
        let hook_name = {
            let mut chars = page_name.chars();
            match chars.next() {
                Some(c) => format!("use{}{}Registrations", c.to_uppercase(), chars.as_str()),
                None => continue,
            }
        };

        // Try to find the corresponding page component
        let mut page_path: Option<PathBuf> = None;
        for pattern in &[
            format!("src/app/{}/page.tsx", page_name),
            format!("src/app/{}/page.jsx", page_name),
            format!("src/pages/{}/index.tsx", page_name),
            format!("src/pages/{}.tsx", page_name),
            format!("app/{}/page.tsx", page_name),
            format!("pages/{}/index.tsx", page_name),
        ] {
            let candidate = project.join(pattern);
            if candidate.exists() {
                page_path = Some(candidate);
                break;
            }
        }

        let page_path = match page_path {
            Some(p) => p,
            None => {
                warnings.push(format!(
                    "Could not find page component for registration file: {}",
                    reg_file
                ));
                continue;
            }
        };

        // Read the page component
        let content = match tokio::fs::read_to_string(&page_path).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Skip if already imports the hook
        if content.contains(&hook_name) {
            continue;
        }

        // Build the import path (relative from page to registration file)
        let import_path = format!("@/lib/ui-bridge/pages/{}-registrations", page_name);

        let mut new_content = content.clone();

        // Add import statement after the last existing import
        let import_line = format!("import {{ {} }} from '{}';\n", hook_name, import_path);
        let import_pos = find_last_import_pos(&new_content);
        new_content.insert_str(import_pos, &import_line);

        // Add hook call at the start of the component function body
        // Find "export function" or "export default function" followed by "{"
        let hook_call = format!("  const {{ }} = {}();\n", hook_name);
        if let Some(fn_pos) = new_content
            .find("export function ")
            .or_else(|| new_content.find("export default function "))
        {
            // Find the opening { of the function body
            if let Some(brace_pos) = new_content[fn_pos..].find('{') {
                let insert_pos = fn_pos + brace_pos + 1;
                // Insert after the { and its newline
                let after_brace = &new_content[insert_pos..];
                let offset = if after_brace.starts_with('\n') { 1 } else { 0 };
                new_content.insert_str(insert_pos + offset, &hook_call);
            }
        }

        // Write the modified page component
        if let Err(e) = tokio::fs::write(&page_path, &new_content).await {
            warnings.push(format!(
                "Failed to add {} import to {}: {}",
                hook_name,
                page_path.display(),
                e
            ));
        } else {
            warnings.push(format!(
                "Added {} import to {}",
                hook_name,
                page_path
                    .strip_prefix(project)
                    .unwrap_or(&page_path)
                    .display()
            ));
        }
    }

    warnings
}

// ============================================================================
// Post-Apply: Auto-update tutorial registry
// ============================================================================

/// After writing tutorial .ts files, update the tutorial registry (data/index.ts)
/// to import and register the new tutorials.
async fn update_tutorial_registry(
    project: &std::path::Path,
    written_files: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Find tutorial files among written files
    let tutorial_files: Vec<&str> = written_files
        .iter()
        .filter(|f| f.contains("tutorial/data/") && f.ends_with(".ts") && !f.ends_with("index.ts"))
        .map(|f| f.as_str())
        .collect();

    if tutorial_files.is_empty() {
        return warnings;
    }

    // Find the tutorial registry file
    let registry_path = project.join("src/components/tutorial/data/index.ts");
    if !registry_path.exists() {
        warnings.push(
            "Tutorial registry not found at src/components/tutorial/data/index.ts".to_string(),
        );
        return warnings;
    }

    let content = match tokio::fs::read_to_string(&registry_path).await {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("Failed to read tutorial registry: {}", e));
            return warnings;
        }
    };

    let mut new_content = content.clone();
    let mut added_count = 0;

    for tutorial_file in &tutorial_files {
        // Extract the tutorial name from file path
        // e.g., "src/components/tutorial/data/dashboard.ts" → "dashboard"
        let file_stem = std::path::Path::new(tutorial_file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        if file_stem.is_empty() {
            continue;
        }

        // Build the export name: "dashboardTutorial"
        let export_name = {
            let parts: Vec<&str> = file_stem.split('-').collect();
            let mut name = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i == 0 {
                    name.push_str(part);
                } else {
                    let mut chars = part.chars();
                    if let Some(c) = chars.next() {
                        name.push(c.to_uppercase().next().unwrap_or(c));
                        name.push_str(chars.as_str());
                    }
                }
            }
            format!("{}Tutorial", name)
        };

        // Skip if already imported
        if new_content.contains(&export_name) {
            continue;
        }

        // Add import statement at the end of imports
        let import_line = format!("import {{ {} }} from \"./{}\";\n", export_name, file_stem);
        let import_pos = find_last_import_pos(&new_content);
        new_content.insert_str(import_pos, &import_line);

        // Add to the tutorials array — find the closing "];" of the array
        if let Some(array_end) = new_content.rfind("];") {
            // Find the last entry before the closing
            let before_close = &new_content[..array_end];
            let insert_pos = if let Some(last_comma) = before_close.rfind(',') {
                last_comma + 1
            } else {
                array_end
            };
            let entry = format!("\n  {},", export_name);
            new_content.insert_str(insert_pos, &entry);
        }

        added_count += 1;
    }

    if added_count > 0 {
        if let Err(e) = tokio::fs::write(&registry_path, &new_content).await {
            warnings.push(format!("Failed to update tutorial registry: {}", e));
        } else {
            warnings.push(format!(
                "Added {} tutorial(s) to registry at src/components/tutorial/data/index.ts",
                added_count
            ));
        }
    }

    warnings
}

// ============================================================================
// Read File Handler
// ============================================================================

async fn handle_read_file(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ReadFileRequest>,
) -> Json<ApiResponse<String>> {
    let project = PathBuf::from(&req.project_path);
    let file_path = project.join(&req.file_path);

    if !file_path.exists() {
        return Json(ApiResponse::error(format!(
            "File does not exist: {}",
            req.file_path
        )));
    }

    match tokio::fs::read_to_string(&file_path).await {
        Ok(content) => Json(ApiResponse::success(content)),
        Err(e) => Json(ApiResponse::error(format!(
            "Failed to read {}: {}",
            req.file_path, e
        ))),
    }
}

// ============================================================================
// Runner Project Path
// ============================================================================

/// Returns the runner's own project directory (dev mode only).
async fn handle_runner_project_path() -> Json<ApiResponse<Option<String>>> {
    let path = std::env::current_exe().ok().and_then(|exe| {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d.join("package.json").exists() && d.join("src-tauri").exists() {
                return Some(d.to_string_lossy().to_string());
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    });
    Json(ApiResponse::success(path))
}

// ============================================================================
// Cache Architecture Spec
// ============================================================================

#[derive(Debug, Deserialize)]
struct CacheArchitectureSpecRequest {
    project_path: String,
    spec_json: String,
}

/// Caches an architecture spec in the cached_app_specs table so it appears
/// in the Architecture page's SDK Projects view.
async fn handle_cache_architecture_spec(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CacheArchitectureSpecRequest>,
) -> Json<ApiResponse<serde_json::Value>> {
    let db = &state.app_state.pg_db;

    // Normalize the project path: canonicalize resolves "..", symlinks, and
    // produces an absolute path.  Fall back to the raw input if canonicalization
    // fails (e.g. the path doesn't exist on this machine).
    let normalized_path = std::path::Path::new(&req.project_path)
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from(&req.project_path));

    // Derive a project name from the normalized path
    let project_name = normalized_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown Project".to_string());

    // Use normalized project path as the "app_url" key for local file-based specs
    let normalized_str = normalized_path.to_string_lossy().replace('\\', "/");
    let app_url = format!("file://{}", normalized_str);
    let spec_id = format!(
        "{}.architecture",
        project_name.to_lowercase().replace(' ', "-")
    );

    if let Err(e) = db
        .upsert_cached_spec(&app_url, &project_name, &spec_id, &req.spec_json, None)
        .await
    {
        return Json(ApiResponse::error(format!("Failed to cache spec: {}", e)));
    }

    Json(ApiResponse::success(serde_json::json!({
        "cached": true,
        "app_url": app_url,
        "spec_id": spec_id,
    })))
}

// ============================================================================
// ============================================================================
// Page Discovery & Source Reading (AI-powered preparation)
// ============================================================================

#[derive(Debug, Clone, Serialize)]
struct PageComponent {
    route: String,
    component_path: String,
    component_name: String,
    has_registrations: bool,
    has_data_page_id: bool,
    has_spec: bool,
    has_tutorial: bool,
}

#[derive(Debug, Deserialize)]
struct DiscoverPagesRequest {
    project_path: String,
}

#[derive(Debug, Serialize)]
struct DiscoverPagesResult {
    pages: Vec<PageComponent>,
    framework: Framework,
}

/// Walk a project and find all page/route components.
async fn discover_pages(project_path: &str) -> Result<DiscoverPagesResult, String> {
    let project = PathBuf::from(project_path);
    if !project.exists() {
        return Err(format!("Project path does not exist: {}", project_path));
    }

    let analysis = analyze_project(project_path).await?;
    let mut pages = Vec::new();

    match analysis.framework {
        Framework::NextJs => {
            // App Router: app/**/page.tsx
            discover_nextjs_pages(&project, &mut pages).await;
        }
        Framework::React => {
            // CRA/Vite: src/pages/**/*.tsx or src/app/**/*.tsx
            discover_react_pages(&project, &mut pages).await;
        }
        _ => {
            // Generic: scan for common page patterns
            discover_generic_pages(&project, &mut pages).await;
        }
    }

    // Tauri apps are typically tab-based (no route files) — their convention
    // is `src/**/<Name>Page.tsx`. If `src-tauri/` exists, scan for that
    // pattern in addition to whatever the framework-specific discovery
    // already found (they're deduped below).
    if project.join("src-tauri").exists() {
        discover_tauri_pages(&project, &mut pages).await;
    }

    // Deduplicate pages by component_path
    pages.sort_by(|a, b| a.component_path.cmp(&b.component_path));
    pages.dedup_by(|a, b| a.component_path == b.component_path);

    // Check existing artifacts for each page
    for page in &mut pages {
        let page_dir = project
            .join(&page.component_path)
            .parent()
            .map(|p| p.to_path_buf());
        let page_stem = std::path::Path::new(&page.component_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("page");

        // Check for useUIElement/useUIComponent and data-page-id in the file
        if let Ok(content) = tokio::fs::read_to_string(project.join(&page.component_path)).await {
            page.has_registrations =
                content.contains("useUIElement") || content.contains("useUIComponent");
            page.has_data_page_id = content.contains("data-page-id");
        }

        // Check for .spec.uibridge.json nearby or in specs/
        let spec_name = format!(
            "{}.spec.uibridge.json",
            page.route.trim_start_matches('/').replace('/', "-")
        );
        if let Some(ref dir) = page_dir {
            page.has_spec = dir.join(&spec_name).exists();
        }
        if !page.has_spec {
            // Also check project-level specs directory
            page.has_spec = project.join("src/specs").join(&spec_name).exists()
                || project.join("specs").join(&spec_name).exists();
        }

        // Check for tutorial data file
        let tutorial_name = format!(
            "{}.ts",
            page.route.trim_start_matches('/').replace('/', "-")
        );
        page.has_tutorial = project
            .join("src/components/tutorial/data")
            .join(&tutorial_name)
            .exists();
    }

    Ok(DiscoverPagesResult {
        pages,
        framework: analysis.framework,
    })
}

/// Discover pages in a Next.js App Router project.
async fn discover_nextjs_pages(project: &PathBuf, pages: &mut Vec<PageComponent>) {
    let app_dir = if project.join("src/app").exists() {
        project.join("src/app")
    } else if project.join("app").exists() {
        project.join("app")
    } else {
        return;
    };

    walk_for_pages(
        &app_dir,
        project,
        pages,
        &["page.tsx", "page.jsx", "page.ts"],
    )
    .await;

    // Also check pages/ directory (Pages Router)
    let pages_dir = if project.join("src/pages").exists() {
        Some(project.join("src/pages"))
    } else if project.join("pages").exists() {
        Some(project.join("pages"))
    } else {
        None
    };

    if let Some(dir) = pages_dir {
        walk_for_pages(&dir, project, pages, &["index.tsx", "index.jsx"]).await;
    }
}

/// Discover pages in a React (CRA/Vite) project.
async fn discover_react_pages(project: &PathBuf, pages: &mut Vec<PageComponent>) {
    for dir_name in &["src/pages", "src/app", "src/views", "src/routes"] {
        let dir = project.join(dir_name);
        if dir.exists() {
            walk_for_pages(
                &dir,
                project,
                pages,
                &["index.tsx", "index.jsx", "page.tsx", "page.jsx"],
            )
            .await;
        }
    }
}

/// Discover pages generically (scan common patterns).
async fn discover_generic_pages(project: &PathBuf, pages: &mut Vec<PageComponent>) {
    discover_react_pages(project, pages).await;
    // Also check Next.js patterns
    discover_nextjs_pages(project, pages).await;
}

/// Discover pages in a Tauri (tab-based) app. The convention is files named
/// `<Something>Page.tsx` anywhere under `src/` — these back tab entries in a
/// TabContent switch rather than URL routes. Route slugs are derived from
/// the filename (PascalCase stripped of the `Page` suffix → kebab-case).
async fn discover_tauri_pages(project: &PathBuf, pages: &mut Vec<PageComponent>) {
    let src_dir = project.join("src");
    if !src_dir.exists() {
        return;
    }
    let mut stack = vec![src_dir];
    while let Some(current) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(e) => e,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            // Skip noise / generated output.
            if file_name.starts_with('.')
                || file_name == "node_modules"
                || file_name == "dist"
                || file_name == "build"
                || file_name == "specs"
            {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            // Match <Name>Page.tsx / <Name>Page.jsx but not test/stories files.
            let matches_page = (file_name.ends_with("Page.tsx") || file_name.ends_with("Page.jsx"))
                && !file_name.ends_with(".test.tsx")
                && !file_name.ends_with(".test.jsx")
                && !file_name.ends_with(".stories.tsx")
                && !file_name.ends_with(".stories.jsx");
            if !matches_page {
                continue;
            }
            let rel_path = path
                .strip_prefix(project)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let component_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Page")
                .to_string();
            let route = pascal_name_to_route(&component_name);
            pages.push(PageComponent {
                route,
                component_path: rel_path,
                component_name,
                has_registrations: false,
                has_data_page_id: false,
                has_spec: false,
                has_tutorial: false,
            });
        }
    }
}

/// Convert a PascalCase component name (typically ending in `Page`) into a
/// route slug. Examples:
///   - "PromptHomePage" -> "/prompt-home"
///   - "UIBridgeIntegrationPage" -> "/ui-bridge-integration"
///   - "ProjectExplainerPage" -> "/project-explainer"
fn pascal_name_to_route(name: &str) -> String {
    let trimmed = name.strip_suffix("Page").unwrap_or(name);
    if trimmed.is_empty() {
        return "/".to_string();
    }
    // Insert a `-` before each uppercase letter that follows a lowercase one
    // OR a sequence boundary within an acronym (e.g. `UIBridge` -> `ui-bridge`).
    let mut out = String::with_capacity(trimmed.len() + 4);
    let chars: Vec<char> = trimmed.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            // Split on lower-to-upper boundary, and on acronym-to-word
            // boundary (e.g. `UIB` -> `ui-b` when followed by lowercase).
            let insert_dash = prev.is_ascii_lowercase()
                || prev.is_ascii_digit()
                || (prev.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase()));
            if insert_dash {
                out.push('-');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    format!("/{}", out)
}

#[cfg(test)]
mod pascal_route_tests {
    use super::pascal_name_to_route;
    #[test]
    fn converts_page_names() {
        assert_eq!(pascal_name_to_route("PromptHomePage"), "/prompt-home");
        assert_eq!(
            pascal_name_to_route("UIBridgeIntegrationPage"),
            "/ui-bridge-integration"
        );
        assert_eq!(
            pascal_name_to_route("ProjectExplainerPage"),
            "/project-explainer"
        );
        assert_eq!(pascal_name_to_route("HomePage"), "/home");
        assert_eq!(pascal_name_to_route("Page"), "/");
    }
}

/// Recursively walk a directory for page files.
async fn walk_for_pages(
    dir: &PathBuf,
    project_root: &PathBuf,
    pages: &mut Vec<PageComponent>,
    page_filenames: &[&str],
) {
    let mut stack = vec![dir.clone()];

    while let Some(current) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&current).await {
            Ok(e) => e,
            Err(_) => continue,
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let file_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            // Skip node_modules, .next, etc.
            if file_name.starts_with('.') || file_name == "node_modules" || file_name == ".next" {
                continue;
            }

            if path.is_dir() {
                stack.push(path);
            } else if page_filenames.iter().any(|f| file_name == *f) {
                let rel_path = path
                    .strip_prefix(project_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");

                // Derive route from path
                let route = derive_route_from_path(&rel_path);
                let component_name = derive_component_name(&rel_path);

                pages.push(PageComponent {
                    route,
                    component_path: rel_path,
                    component_name,
                    has_registrations: false,
                    has_data_page_id: false,
                    has_spec: false,
                    has_tutorial: false,
                });
            }
        }
    }
}

/// Derive a route path from a file path (e.g., "src/app/dashboard/page.tsx" -> "/dashboard").
fn derive_route_from_path(rel_path: &str) -> String {
    let path = rel_path
        .replace('\\', "/")
        .trim_start_matches("src/")
        .trim_start_matches("app/")
        .trim_start_matches("pages/")
        .to_string();

    let route = path
        .trim_end_matches("/page.tsx")
        .trim_end_matches("/page.jsx")
        .trim_end_matches("/page.ts")
        .trim_end_matches("/index.tsx")
        .trim_end_matches("/index.jsx")
        .trim_end_matches(".tsx")
        .trim_end_matches(".jsx");

    if route.is_empty() || route == "." {
        "/".to_string()
    } else {
        format!("/{}", route)
    }
}

/// Derive a component name from a file path.
fn derive_component_name(rel_path: &str) -> String {
    let normalized = rel_path.replace('\\', "/");
    let parts: Vec<&str> = normalized.split('/').collect();
    // Use the directory name for page.tsx/index.tsx, or file stem otherwise
    if parts.len() >= 2 {
        let dir = parts[parts.len() - 2];
        let mut chars = dir.chars();
        match chars.next() {
            Some(c) => format!("{}{}Page", c.to_uppercase(), chars.as_str()),
            None => "Page".to_string(),
        }
    } else {
        "Page".to_string()
    }
}

// ---- Read Page Source ----

#[derive(Debug, Deserialize)]
struct ReadPageSourceRequest {
    project_path: String,
    component_path: String,
    #[serde(default = "default_max_depth")]
    max_depth: u32,
}

fn default_max_depth() -> u32 {
    2
}

#[derive(Debug, Serialize)]
struct ImportedFile {
    path: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct ReadPageSourceResult {
    main_source: String,
    imported_sources: Vec<ImportedFile>,
    total_lines: usize,
}

/// Read a page component and its imported child components.
async fn read_page_source(
    project_path: &str,
    component_path: &str,
    max_depth: u32,
) -> Result<ReadPageSourceResult, String> {
    let project = PathBuf::from(project_path);
    let main_path = project.join(component_path);

    let main_source = tokio::fs::read_to_string(&main_path)
        .await
        .map_err(|e| format!("Failed to read {}: {}", component_path, e))?;

    let mut imported_sources = Vec::new();
    let mut total_size = main_source.len();
    const MAX_TOTAL_SIZE: usize = 50_000; // 50KB cap

    if max_depth > 0 {
        // Extract relative imports
        let imports = extract_local_imports(&main_source);
        let component_dir = main_path.parent().unwrap_or(&project);

        for import_path in imports {
            if total_size >= MAX_TOTAL_SIZE {
                break;
            }

            // Resolve the import to an actual file
            if let Some(resolved) = resolve_import(component_dir, &project, &import_path).await {
                if let Ok(content) = tokio::fs::read_to_string(&resolved).await {
                    let rel = resolved
                        .strip_prefix(&project)
                        .unwrap_or(&resolved)
                        .to_string_lossy()
                        .replace('\\', "/");

                    total_size += content.len();
                    imported_sources.push(ImportedFile { path: rel, content });
                }
            }
        }
    }

    let total_lines = main_source.lines().count()
        + imported_sources
            .iter()
            .map(|f| f.content.lines().count())
            .sum::<usize>();

    Ok(ReadPageSourceResult {
        main_source,
        imported_sources,
        total_lines,
    })
}

/// Extract local relative import paths from TypeScript/JavaScript source.
fn extract_local_imports(source: &str) -> Vec<String> {
    let mut imports = Vec::new();
    // Match: import ... from './path' or import ... from '../path' or from '@/path'
    let re = regex::Regex::new(r#"from\s+['"](\./[^'"]+|\.{2}/[^'"]+|@/[^'"]+)['"]"#).unwrap();
    for cap in re.captures_iter(source) {
        if let Some(path) = cap.get(1) {
            let p = path.as_str().to_string();
            // Skip node_modules-style imports and css/json
            if !p.ends_with(".css")
                && !p.ends_with(".json")
                && !p.ends_with(".svg")
                && !p.ends_with(".png")
            {
                imports.push(p);
            }
        }
    }
    imports
}

/// Resolve TypeScript path aliases from tsconfig.json.
/// Returns the filesystem base directory for a given alias prefix (e.g., "@/" → "src/").
fn resolve_ts_alias(project_root: &PathBuf, alias_prefix: &str) -> Option<PathBuf> {
    // Try tsconfig.json, then tsconfig.app.json
    for config_name in &["tsconfig.json", "tsconfig.app.json"] {
        let config_path = project_root.join(config_name);
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            // Strip JSON comments (// and /* */) for parsing
            let stripped: String = content
                .lines()
                .map(|line| {
                    if let Some(pos) = line.find("//") {
                        &line[..pos]
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");

            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stripped) {
                if let Some(paths) = parsed
                    .get("compilerOptions")
                    .and_then(|co| co.get("paths"))
                    .and_then(|p| p.as_object())
                {
                    // Look for alias matching the prefix (e.g., "@/*" for "@/")
                    let alias_pattern = format!("{}*", alias_prefix);
                    if let Some(targets) = paths.get(&alias_pattern).and_then(|v| v.as_array()) {
                        if let Some(first) = targets.first().and_then(|v| v.as_str()) {
                            // Strip trailing /* to get the directory
                            let dir = first.trim_end_matches("/*").trim_end_matches('*');
                            return Some(project_root.join(dir));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Resolve an import path to an actual file on disk.
async fn resolve_import(
    from_dir: &std::path::Path,
    project_root: &PathBuf,
    import_path: &str,
) -> Option<PathBuf> {
    let base = if import_path.starts_with("@/") {
        // TypeScript path alias — resolve from tsconfig.json paths, fallback to src/
        let alias_base =
            resolve_ts_alias(project_root, "@/").unwrap_or_else(|| project_root.join("src"));
        alias_base.join(&import_path[2..])
    } else if import_path.starts_with("~/") {
        // Some projects use ~/ as an alias
        let alias_base =
            resolve_ts_alias(project_root, "~/").unwrap_or_else(|| project_root.join("src"));
        alias_base.join(&import_path[2..])
    } else {
        from_dir.join(import_path)
    };

    // Try extensions in order
    for ext in &[".tsx", ".ts", ".jsx", ".js", "/index.tsx", "/index.ts"] {
        let candidate = PathBuf::from(format!("{}{}", base.display(), ext));
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // Try the path as-is (if it already has an extension)
    if base.exists() && base.is_file() {
        return Some(base);
    }

    None
}

// ---- HTTP Handlers ----

async fn handle_discover_pages(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<DiscoverPagesRequest>,
) -> Json<ApiResponse<DiscoverPagesResult>> {
    match discover_pages(&req.project_path).await {
        Ok(result) => Json(ApiResponse::success(result)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_read_page_source(
    State(_state): State<Arc<ApiState>>,
    Json(req): Json<ReadPageSourceRequest>,
) -> Json<ApiResponse<ReadPageSourceResult>> {
    match read_page_source(&req.project_path, &req.component_path, req.max_depth).await {
        Ok(result) => Json(ApiResponse::success(result)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

// ============================================================================
// Router
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/integration/analyze", post(handle_analyze))
        .route("/ui-bridge/integration/integrate", post(handle_integrate))
        .route("/ui-bridge/integration/update", post(handle_update))
        .route("/ui-bridge/integration/preview", post(handle_preview))
        .route(
            "/ui-bridge/integration/write-hooks",
            post(handle_write_hooks),
        )
        .route("/ui-bridge/integration/read-file", post(handle_read_file))
        .route(
            "/ui-bridge/integration/runner-project",
            get(handle_runner_project_path),
        )
        .route("/ui-bridge/integration/status", get(handle_status))
        .route(
            "/ui-bridge/integration/health-check",
            post(handle_health_check),
        )
        .route(
            "/ui-bridge/integration/cache-architecture-spec",
            post(handle_cache_architecture_spec),
        )
        .route(
            "/ui-bridge/integration/{id}",
            delete(handle_delete_integration),
        )
        // AI-powered page preparation
        .route(
            "/ui-bridge/integration/discover-pages",
            post(handle_discover_pages),
        )
        .route(
            "/ui-bridge/integration/read-page-source",
            post(handle_read_page_source),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_port_next_dev_with_port_flag() {
        assert_eq!(extract_port_from_script("next dev --port 3001"), Some(3001));
    }

    #[test]
    fn extract_port_vite_equals_syntax() {
        assert_eq!(extract_port_from_script("vite --port=5174"), Some(5174));
    }

    #[test]
    fn extract_port_env_var_style() {
        assert_eq!(
            extract_port_from_script("PORT=8080 node server.js"),
            Some(8080)
        );
    }

    #[test]
    fn extract_port_short_flag() {
        assert_eq!(extract_port_from_script("-p 4000"), Some(4000));
    }

    #[test]
    fn extract_port_no_port_specified() {
        assert_eq!(extract_port_from_script("next dev"), None);
    }

    #[test]
    fn extract_port_empty_string() {
        assert_eq!(extract_port_from_script(""), None);
    }

    #[test]
    fn extract_port_short_flag_equals() {
        assert_eq!(extract_port_from_script("-p=9000"), Some(9000));
    }
}
