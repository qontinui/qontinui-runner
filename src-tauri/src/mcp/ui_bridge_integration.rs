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
    pub server_adapter: Option<String>,
    pub dev_server_port: Option<u16>,
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
pub struct UpdateRequest {
    pub project_path: String,
    pub sdk_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    let mut server_adapter = None;
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

    Ok(ProjectAnalysis {
        project_path: path.to_string(),
        framework,
        package_manager,
        entry_points,
        ui_bridge_status,
        existing_sdk_version,
        has_babel_plugin,
        has_swc_plugin,
        server_adapter,
        dev_server_port,
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

    // Apply file modifications first (so package.json is written before install)
    let mut success = true;
    for modification in &modifications {
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

// ============================================================================
// Integration Status Tracker (SQLite)
// ============================================================================

fn save_integration(
    db: &crate::database::CheckpointDb,
    integration: &TrackedIntegration,
) -> Result<(), String> {
    let conn = db.get_conn_string()?;
    conn.execute(
        r#"INSERT OR REPLACE INTO ui_bridge_integrations
            (id, project_path, label, framework, integration_type, sdk_version,
             status, target_url, last_health_check, element_count,
             created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
        rusqlite::params![
            integration.id,
            integration.project_path,
            integration.label,
            integration.framework,
            integration.integration_type,
            integration.sdk_version,
            integration.status,
            integration.target_url,
            integration.last_health_check,
            integration.element_count,
            integration.created_at,
            integration.updated_at,
        ],
    )
    .map_err(|e| format!("Failed to save integration: {}", e))?;
    Ok(())
}

fn list_integrations(
    db: &crate::database::CheckpointDb,
) -> Result<Vec<TrackedIntegration>, String> {
    let conn = db.get_conn_string()?;
    let mut stmt = conn
        .prepare(
            r#"SELECT id, project_path, label, framework, integration_type, sdk_version,
                      status, target_url, last_health_check, element_count,
                      created_at, updated_at
               FROM ui_bridge_integrations
               ORDER BY updated_at DESC"#,
        )
        .map_err(|e| format!("Failed to query integrations: {}", e))?;

    let integrations = stmt
        .query_map([], |row| {
            Ok(TrackedIntegration {
                id: row.get(0)?,
                project_path: row.get(1)?,
                label: row.get(2)?,
                framework: row.get(3)?,
                integration_type: row.get(4)?,
                sdk_version: row.get(5)?,
                status: row.get(6)?,
                target_url: row.get(7)?,
                last_health_check: row.get(8)?,
                element_count: row.get(9)?,
                created_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        })
        .map_err(|e| format!("Failed to read integrations: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(integrations)
}

fn delete_integration(db: &crate::database::CheckpointDb, id: &str) -> Result<(), String> {
    let conn = db.get_conn_string()?;
    conn.execute(
        "DELETE FROM ui_bridge_integrations WHERE id = ?1",
        rusqlite::params![id],
    )
    .map_err(|e| format!("Failed to delete integration: {}", e))?;
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
                let _ = save_integration(&state.app_state.checkpoint_db, &integration);
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

            Json(ApiResponse::success(modifications))
        }
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_status(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<TrackedIntegration>>> {
    match list_integrations(&state.app_state.checkpoint_db) {
        Ok(integrations) => Json(ApiResponse::success(integrations)),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_delete_integration(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Json<ApiResponse<String>> {
    match delete_integration(&state.app_state.checkpoint_db, &id) {
        Ok(_) => Json(ApiResponse::success(format!("Integration {} deleted", id))),
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_health_check(
    State(state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<TrackedIntegration>>> {
    let integrations = match list_integrations(&state.app_state.checkpoint_db) {
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
        let _ = save_integration(&state.app_state.checkpoint_db, &integration);
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

    // Apply modifications
    let mut success = true;
    for modification in &modifications {
        let file_path = project.join(&modification.file_path);
        if let Err(e) = tokio::fs::write(&file_path, &modification.new_content).await {
            warnings.push(format!("Failed to write {}: {}", modification.file_path, e));
            success = false;
        }
    }

    // Update tracker
    if success {
        let now = chrono::Utc::now().timestamp();
        let integration_id = format!(
            "source-{}",
            req.project_path.replace(['/', '\\', ':', ' '], "-")
        );
        // Update just the version and timestamp in the existing record
        let conn = state.app_state.checkpoint_db.get_conn_string();
        if let Ok(conn) = conn {
            let _ = conn.execute(
                "UPDATE ui_bridge_integrations SET sdk_version = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![new_version, now, integration_id],
            );
        }
    }

    // Auto-run package install after updating package.json
    let install_cmd = match analysis.package_manager {
        PackageManager::Yarn => "yarn install",
        PackageManager::Pnpm => "pnpm install",
        PackageManager::Bun => "bun install",
        _ => "npm install",
    };

    let mut next_steps = vec!["Restart your dev server".to_string()];
    let pkg_modified = !modifications.is_empty();

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
// Router
// ============================================================================

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/ui-bridge/integration/analyze", post(handle_analyze))
        .route("/ui-bridge/integration/integrate", post(handle_integrate))
        .route("/ui-bridge/integration/update", post(handle_update))
        .route("/ui-bridge/integration/preview", post(handle_preview))
        .route("/ui-bridge/integration/status", get(handle_status))
        .route(
            "/ui-bridge/integration/health-check",
            post(handle_health_check),
        )
        .route(
            "/ui-bridge/integration/:id",
            delete(handle_delete_integration),
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
