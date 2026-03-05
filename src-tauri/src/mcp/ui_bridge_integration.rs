//! UI Bridge Integration Module
//!
//! Provides project analysis, runtime injection proxy, source code integration,
//! and integration status tracking for the UI Bridge SDK.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{Response, StatusCode},
    response::{IntoResponse, Json},
    routing::{delete, get, post},
    Router,
};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::{info, warn};

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
pub struct InjectRequest {
    pub target_url: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InjectResponse {
    pub proxy_url: String,
    pub proxy_port: u16,
    pub target_url: String,
    pub status: String,
}

#[derive(Debug)]
pub struct ProxyInstance {
    pub proxy_url: String,
    pub proxy_port: u16,
    pub target_url: String,
    pub label: String,
    pub status: String,
    pub element_count: Option<u32>,
    pub started_at: i64,
    pub shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProxyInfo {
    pub proxy_url: String,
    pub proxy_port: u16,
    pub target_url: String,
    pub label: String,
    pub status: String,
    pub element_count: Option<u32>,
    pub started_at: i64,
}

#[derive(Debug, Default)]
pub struct ProxyManager {
    pub proxies: HashMap<u16, ProxyInstance>,
    next_port: u16,
}

impl ProxyManager {
    pub fn new() -> Self {
        Self {
            proxies: HashMap::new(),
            next_port: 19000,
        }
    }

    fn allocate_port(&mut self) -> u16 {
        let port = self.next_port;
        // Wrap around to avoid exceeding valid port range
        if self.next_port >= 65500 {
            self.next_port = 19000;
        } else {
            self.next_port += 1;
        }
        // Skip ports already in use
        while self.proxies.contains_key(&self.next_port) {
            self.next_port += 1;
            if self.next_port >= 65500 {
                self.next_port = 19000;
            }
        }
        port
    }
}

// ============================================================================
// Proxy Control State (bridges HTTP control API <-> injected script)
// ============================================================================

#[derive(Debug, Serialize, Clone)]
struct PendingCommand {
    id: String,
    method: String,
    args: Vec<serde_json::Value>,
}

struct ProxyControlState {
    pending: Vec<PendingCommand>,
    waiters: HashMap<String, tokio::sync::oneshot::Sender<serde_json::Value>>,
}

impl ProxyControlState {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            waiters: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ControlResult {
    id: String,
    success: bool,
    #[serde(default)]
    data: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActionRequest {
    action: String,
    #[serde(default)]
    params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct NavigateRequest {
    url: String,
}

#[derive(Debug, Deserialize)]
struct WaitForElementRequest {
    selector: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct QuerySelectorRequest {
    selector: String,
}

/// Module-level proxy manager (shared across all requests)
static PROXY_MANAGER: OnceLock<Arc<Mutex<ProxyManager>>> = OnceLock::new();

fn get_proxy_manager() -> Arc<Mutex<ProxyManager>> {
    PROXY_MANAGER
        .get_or_init(|| Arc::new(Mutex::new(ProxyManager::new())))
        .clone()
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
    #[serde(default = "default_true")]
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
    pub proxy_port: Option<u16>,
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
// Runtime Injection Proxy
// ============================================================================

const INJECT_SCRIPT: &str = include_str!("../../resources/ui-bridge-inject.js");

/// Enqueue a command for the inject script and wait for its result (5s timeout).
async fn enqueue_and_wait(
    control_state: &Arc<Mutex<ProxyControlState>>,
    method: &str,
    args: Vec<serde_json::Value>,
) -> Response<Body> {
    let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    {
        let mut state = control_state.lock().await;
        state.pending.push(PendingCommand {
            id: cmd_id.clone(),
            method: method.to_string(),
            args,
        });
        state.waiters.insert(cmd_id.clone(), tx);
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(value)) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&value).unwrap_or_default(),
            ))
            .unwrap(),
        Ok(Err(_)) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"error":"Command channel closed unexpectedly"}"#,
            ))
            .unwrap(),
        Err(_) => {
            // Timeout — clean up the waiter
            let mut state = control_state.lock().await;
            state.waiters.remove(&cmd_id);
            Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"error":"Timed out waiting for inject script response (5s). Is the proxied page open in a browser?"}"#,
                ))
                .unwrap()
        }
    }
}

/// Like `enqueue_and_wait` but with a configurable timeout (for long-running ops like waitForElement).
async fn enqueue_and_wait_with_timeout(
    control_state: &Arc<Mutex<ProxyControlState>>,
    method: &str,
    args: Vec<serde_json::Value>,
    timeout_secs: u64,
) -> Response<Body> {
    let cmd_id = format!("cmd-{}", uuid::Uuid::new_v4());
    let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();

    {
        let mut state = control_state.lock().await;
        state.pending.push(PendingCommand {
            id: cmd_id.clone(),
            method: method.to_string(),
            args,
        });
        state.waiters.insert(cmd_id.clone(), tx);
    }

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(value)) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&value).unwrap_or_default(),
            ))
            .unwrap(),
        Ok(Err(_)) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"error":"Command channel closed unexpectedly"}"#,
            ))
            .unwrap(),
        Err(_) => {
            let mut state = control_state.lock().await;
            state.waiters.remove(&cmd_id);
            let msg = format!(
                r#"{{"error":"Timed out waiting for inject script response ({}s). Is the proxied page open in a browser?"}}"#,
                timeout_secs
            );
            Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .header("content-type", "application/json")
                .body(Body::from(msg))
                .unwrap()
        }
    }
}

async fn start_proxy(
    target_url: String,
    port: u16,
) -> Result<tokio::sync::oneshot::Sender<()>, String> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let client = Client::new();
    let control_state = Arc::new(Mutex::new(ProxyControlState::new()));

    let ws_target = target_url.clone();
    let target = target_url.clone();

    // Build control routes
    let cs = control_state.clone();
    let pending_handler = move || {
        let cs = cs.clone();
        async move {
            let mut state = cs.lock().await;
            let cmds: Vec<PendingCommand> = state.pending.drain(..).collect();
            Json(cmds)
        }
    };

    let cs = control_state.clone();
    let results_handler = move |Json(results): Json<Vec<ControlResult>>| {
        let cs = cs.clone();
        async move {
            let mut state = cs.lock().await;
            for result in results {
                if let Some(tx) = state.waiters.remove(&result.id) {
                    let value = if result.success {
                        result.data.unwrap_or(serde_json::Value::Null)
                    } else {
                        serde_json::json!({
                            "error": result.error.unwrap_or_else(|| "Unknown error".to_string())
                        })
                    };
                    let _ = tx.send(value);
                }
            }
            Json(serde_json::json!({"ok": true}))
        }
    };

    let cs = control_state.clone();
    let snapshot_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "getSnapshot", vec![]).await }
    };

    let cs = control_state.clone();
    let elements_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "getElements", vec![]).await }
    };

    let cs = control_state.clone();
    let element_handler = move |Path(id): Path<String>| {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "getElement", vec![serde_json::Value::String(id)]).await }
    };

    let cs = control_state.clone();
    let action_handler = move |Path(id): Path<String>, Json(body): Json<ActionRequest>| {
        let cs = cs.clone();
        async move {
            let params = body.params.unwrap_or(serde_json::Value::Null);
            enqueue_and_wait(
                &cs,
                "executeAction",
                vec![
                    serde_json::Value::String(id),
                    serde_json::Value::String(body.action),
                    params,
                ],
            )
            .await
        }
    };

    let cs = control_state.clone();
    let discover_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "discover", vec![]).await }
    };

    let cs = control_state.clone();
    let console_errors_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "getConsoleErrors", vec![]).await }
    };

    let cs = control_state.clone();
    let clear_console_errors_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "clearConsoleErrors", vec![]).await }
    };

    let cs = control_state.clone();
    let navigate_handler = move |Json(body): Json<NavigateRequest>| {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "navigate", vec![serde_json::Value::String(body.url)]).await }
    };

    let cs = control_state.clone();
    let refresh_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "refresh", vec![]).await }
    };

    let cs = control_state.clone();
    let back_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "back", vec![]).await }
    };

    let cs = control_state.clone();
    let forward_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "forward", vec![]).await }
    };

    let cs = control_state.clone();
    let styles_handler = move |Path(id): Path<String>| {
        let cs = cs.clone();
        async move {
            enqueue_and_wait(
                &cs,
                "getComputedStyles",
                vec![serde_json::Value::String(id)],
            )
            .await
        }
    };

    let cs = control_state.clone();
    let accessibility_handler = move |Path(id): Path<String>| {
        let cs = cs.clone();
        async move {
            enqueue_and_wait(
                &cs,
                "getAccessibilityInfo",
                vec![serde_json::Value::String(id)],
            )
            .await
        }
    };

    let cs = control_state.clone();
    let wait_for_element_handler = move |Json(body): Json<WaitForElementRequest>| {
        let cs = cs.clone();
        async move {
            let timeout_ms = body.timeout_ms.unwrap_or(5000);
            // HTTP timeout = inject timeout + 2s buffer
            let http_timeout_secs = (timeout_ms / 1000) + 2;
            enqueue_and_wait_with_timeout(
                &cs,
                "waitForElement",
                vec![
                    serde_json::Value::String(body.selector),
                    serde_json::json!(timeout_ms),
                ],
                http_timeout_secs.max(5),
            )
            .await
        }
    };

    let cs = control_state.clone();
    let query_selector_handler = move |Json(body): Json<QuerySelectorRequest>| {
        let cs = cs.clone();
        async move {
            enqueue_and_wait(
                &cs,
                "querySelectorAll",
                vec![serde_json::Value::String(body.selector)],
            )
            .await
        }
    };

    let cs = control_state.clone();
    let design_snapshot_handler = move || {
        let cs = cs.clone();
        async move { enqueue_and_wait(&cs, "getDesignSnapshot", vec![]).await }
    };

    let proxy_router = Router::new()
        .route(
            "/__ui-bridge/inject.js",
            get(|| async {
                Response::builder()
                    .header("content-type", "application/javascript")
                    .header("cache-control", "no-cache")
                    .body(Body::from(INJECT_SCRIPT))
                    .unwrap()
            }),
        )
        .route(
            "/__ui-bridge/health",
            get(|| async {
                Json(serde_json::json!({
                    "status": "ok",
                    "injected": true,
                    "version": "1.0.0"
                }))
            }),
        )
        // Control API: polled by inject script
        .route("/__ui-bridge/control/pending", get(pending_handler))
        .route("/__ui-bridge/control/results", post(results_handler))
        // Control API: called by external tools (MCP, workflows, CLI)
        .route("/__ui-bridge/control/snapshot", get(snapshot_handler))
        .route("/__ui-bridge/control/elements", get(elements_handler))
        .route("/__ui-bridge/control/element/:id", get(element_handler))
        .route(
            "/__ui-bridge/control/element/:id/action",
            post(action_handler),
        )
        .route("/__ui-bridge/control/discover", post(discover_handler))
        .route(
            "/__ui-bridge/control/console-errors",
            get(console_errors_handler),
        )
        .route(
            "/__ui-bridge/control/console-errors/clear",
            post(clear_console_errors_handler),
        )
        .route("/__ui-bridge/control/page/navigate", post(navigate_handler))
        .route("/__ui-bridge/control/page/refresh", post(refresh_handler))
        .route("/__ui-bridge/control/page/back", post(back_handler))
        .route("/__ui-bridge/control/page/forward", post(forward_handler))
        // Enriched control routes
        .route(
            "/__ui-bridge/control/element/:id/styles",
            get(styles_handler),
        )
        .route(
            "/__ui-bridge/control/element/:id/accessibility",
            get(accessibility_handler),
        )
        .route(
            "/__ui-bridge/control/wait-for-element",
            post(wait_for_element_handler),
        )
        .route(
            "/__ui-bridge/control/query-selector",
            post(query_selector_handler),
        )
        .route(
            "/__ui-bridge/control/design-snapshot",
            get(design_snapshot_handler),
        )
        .fallback(move |req: axum::http::Request<Body>| {
            let client = client.clone();
            let target = target.clone();
            let ws_target = ws_target.clone();
            async move {
                // Check for WebSocket upgrade requests (e.g., HMR)
                let is_ws_upgrade = req
                    .headers()
                    .get("upgrade")
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.eq_ignore_ascii_case("websocket"))
                    .unwrap_or(false);

                if is_ws_upgrade {
                    proxy_websocket(ws_target, req).await
                } else {
                    proxy_request(client, target, req).await
                }
            }
        });

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind proxy on port {}: {}", port, e))?;

    info!(
        "UI Bridge injection proxy started: http://localhost:{} -> {}",
        port, target_url
    );

    tokio::spawn(async move {
        let server = axum::serve(listener, proxy_router);
        tokio::select! {
            result = server => {
                if let Err(e) = result {
                    warn!("Proxy server error on port {}: {}", port, e);
                }
            }
            _ = shutdown_rx => {
                info!("Proxy on port {} shutting down", port);
            }
        }
    });

    Ok(shutdown_tx)
}

/// Handle WebSocket upgrade requests by proxying them bidirectionally to the target app.
/// Used for HMR/hot reload connections from dev servers (Vite, webpack, Next.js, etc.).
async fn proxy_websocket(target_base: String, req: axum::http::Request<Body>) -> Response<Body> {
    use axum::extract::FromRequestParts;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let uri = req.uri().clone();
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    // Convert http:// to ws://
    let ws_url = format!(
        "ws{}",
        target_base
            .trim_end_matches('/')
            .strip_prefix("http")
            .unwrap_or(&target_base)
    );
    let target_ws_url = format!("{}{}", ws_url, path_and_query);

    // Split the request into parts and body so we can extract the WebSocketUpgrade
    let (mut parts, _body) = req.into_parts();

    // Extract WebSocketUpgrade from request parts
    let ws = match axum::extract::ws::WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!("WebSocket upgrade failed: {}", e);
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!("WebSocket upgrade error: {}", e)))
                .unwrap();
        }
    };

    ws.on_upgrade(move |mut client_ws| async move {
        // Connect to the upstream WebSocket
        let upstream = match tokio_tungstenite::connect_async(&target_ws_url).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                warn!("WebSocket upstream connection failed to {}: {}", target_ws_url, e);
                return;
            }
        };

        let (mut upstream_write, mut upstream_read) = upstream.split();

        // Bidirectional relay between client and upstream
        loop {
            tokio::select! {
                msg = client_ws.recv() => {
                    match msg {
                        Some(Ok(axum::extract::ws::Message::Text(text))) => {
                            if upstream_write.send(Message::Text(text.to_string())).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(axum::extract::ws::Message::Binary(data))) => {
                            if upstream_write.send(Message::Binary(data.to_vec())).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
                msg = upstream_read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if client_ws.send(axum::extract::ws::Message::Text(text)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Binary(data))) => {
                            if client_ws.send(axum::extract::ws::Message::Binary(data)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        _ => {}
                    }
                }
            }
        }
        let _ = upstream_write.close().await;
    })
    .into_response()
}

async fn proxy_request(
    client: Client,
    target_base: String,
    req: axum::http::Request<Body>,
) -> Response<Body> {
    let uri = req.uri().clone();
    let method = req.method().clone();
    let headers = req.headers().clone();
    let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");

    let target_url = format!("{}{}", target_base.trim_end_matches('/'), path_and_query);

    let body_bytes = match axum::body::to_bytes(req.into_body(), 50 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(format!("Failed to read request body: {}", e)))
                .unwrap();
        }
    };

    let mut proxy_req = client.request(method, &target_url);
    for (key, value) in headers.iter() {
        let key_str = key.as_str();
        if key_str == "host"
            || key_str == "connection"
            || key_str == "transfer-encoding"
            || key_str == "keep-alive"
        {
            continue;
        }
        proxy_req = proxy_req.header(key, value);
    }
    if !body_bytes.is_empty() {
        proxy_req = proxy_req.body(body_bytes.to_vec());
    }

    let proxy_resp = match proxy_req.send().await {
        Ok(resp) => resp,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("Proxy error: {}", e)))
                .unwrap();
        }
    };

    let status = proxy_resp.status();
    let resp_headers = proxy_resp.headers().clone();
    let content_type = resp_headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let is_html = content_type.contains("text/html");

    // Check if response is gzip-encoded
    let is_gzip = resp_headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("gzip"))
        .unwrap_or(false);

    let resp_bytes = match proxy_resp.bytes().await {
        Ok(bytes) => bytes,
        Err(e) => {
            return Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("Failed to read proxy response: {}", e)))
                .unwrap();
        }
    };

    // Check if the charset is something we can't safely inject into
    let is_utf8_compatible = {
        let charset = content_type
            .split(';')
            .find_map(|part| {
                let part = part.trim();
                part.strip_prefix("charset=")
                    .map(|charset| charset.trim().to_lowercase())
            })
            .unwrap_or_else(|| "utf-8".to_string());
        charset == "utf-8" || charset == "us-ascii" || charset == "ascii" || charset == "iso-8859-1"
    };

    let final_body = if is_html && is_utf8_compatible {
        // For HTML responses, decompress if gzipped before injecting
        let html_bytes = if is_gzip {
            let mut decoder = GzDecoder::new(&resp_bytes[..]);
            let mut decompressed = Vec::new();
            match decoder.read_to_end(&mut decompressed) {
                Ok(_) => decompressed,
                Err(_) => resp_bytes.to_vec(), // Fallback to raw bytes if decompression fails
            }
        } else {
            resp_bytes.to_vec()
        };

        let html = String::from_utf8_lossy(&html_bytes);
        let inject_tag = r#"<script src="/__ui-bridge/inject.js"></script>"#;
        let injected = if let Some(pos) = html.find("</head>") {
            format!("{}{}\n{}", &html[..pos], inject_tag, &html[pos..])
        } else if let Some(pos) = html.find("<body") {
            if let Some(close) = html[pos..].find('>') {
                let insert_pos = pos + close + 1;
                format!(
                    "{}\n{}\n{}",
                    &html[..insert_pos],
                    inject_tag,
                    &html[insert_pos..]
                )
            } else {
                format!("{}\n{}", inject_tag, html)
            }
        } else {
            format!("{}\n{}", inject_tag, html)
        };
        Body::from(injected)
    } else {
        Body::from(resp_bytes)
    };

    let injected_html = is_html && is_utf8_compatible;
    let mut response = Response::builder().status(status);
    for (key, value) in resp_headers.iter() {
        let key_str = key.as_str();
        if key_str == "connection"
            || key_str == "transfer-encoding"
            || key_str == "keep-alive"
            || (injected_html && key_str == "content-length")
            || (injected_html && key_str == "content-encoding")
        {
            continue;
        }
        response = response.header(key, value);
    }
    response.body(final_body).unwrap()
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
        }
        Framework::Vue | Framework::Angular | Framework::Svelte => {
            integrate_generic_html(&project, &mut modifications, &mut warnings).await;
            next_steps.push("Restart your dev server to apply changes".to_string());
            warnings.push(format!(
                "{:?} integration uses script injection. Full SDK integration is only available for React/Next.js.",
                analysis.framework
            ));
        }
        Framework::PlainHtml => {
            integrate_plain_html(&project, &mut modifications).await;
            next_steps.push("Refresh your browser to see changes".to_string());
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

    // Auto-instrument: add babel plugin to devDependencies
    if options.auto_instrument && !analysis.has_babel_plugin {
        add_babel_plugin_to_package_json(project, modifications).await;
    }

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
                    description: "Wrap root component with UIBridgeProvider".to_string(),
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
    add_server_to_package_json(project, modifications).await;

    // Auto-instrument: add babel plugin and configure in next.config
    if options.auto_instrument && !analysis.has_babel_plugin {
        add_babel_plugin_to_package_json(project, modifications).await;
        add_babel_to_next_config(project, modifications, warnings).await;
    }

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
                    description: "Wrap root layout with UIBridgeProvider".to_string(),
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

    let api_route_path = if project.join("src/app").exists() {
        "src/app/api/ui-bridge/[...path]/route.ts"
    } else {
        "app/api/ui-bridge/[...path]/route.ts"
    };

    if !project.join(api_route_path).exists() {
        modifications.push(FileModification {
            file_path: api_route_path.to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create Next.js API route for UI Bridge server adapter".to_string(),
            original_content: None,
            new_content: NEXTJS_API_ROUTE_TEMPLATE.to_string(),
        });
    }
}

async fn integrate_generic_html(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    let candidates = [
        project.join("index.html"),
        project.join("src/index.html"),
        project.join("public/index.html"),
    ];
    let html_path = candidates.iter().find(|p| p.exists());

    if let Some(path) = html_path {
        if let Ok(content) = tokio::fs::read_to_string(path).await {
            if !content.contains("ui-bridge-inject.js") && !content.contains("@qontinui/ui-bridge")
            {
                let inject_tag = "<script src=\"/__ui-bridge/inject.js\"></script>";
                let new_content = if let Some(pos) = content.find("</head>") {
                    format!("{}    {}\n{}", &content[..pos], inject_tag, &content[pos..])
                } else {
                    format!("{}\n{}", inject_tag, content)
                };
                let rel_path = path
                    .strip_prefix(project)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                modifications.push(FileModification {
                    file_path: rel_path,
                    modification_type: ModificationType::Replace,
                    description: "Inject UI Bridge script into HTML".to_string(),
                    original_content: Some(content),
                    new_content,
                });
            }
        }
    } else {
        warnings.push("No index.html found for script injection.".to_string());
    }
}

async fn integrate_plain_html(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
) {
    let index_path = project.join("index.html");
    if let Ok(content) = tokio::fs::read_to_string(&index_path).await {
        if !content.contains("ui-bridge-inject.js") {
            let inject_tag = r#"<script src="/__ui-bridge/inject.js"></script>"#;
            let new_content = if let Some(pos) = content.find("</head>") {
                format!("{}    {}\n{}", &content[..pos], inject_tag, &content[pos..])
            } else {
                format!("{}\n{}", inject_tag, content)
            };
            modifications.push(FileModification {
                file_path: "index.html".to_string(),
                modification_type: ModificationType::Replace,
                description: "Inject UI Bridge script into index.html".to_string(),
                original_content: Some(content),
                new_content,
            });
        }
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

async fn add_server_to_package_json(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
) {
    let pkg_path = project.join("package.json");
    if let Ok(content) = tokio::fs::read_to_string(&pkg_path).await {
        if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            let deps = pkg.get_mut("dependencies").and_then(|d| d.as_object_mut());
            if let Some(deps) = deps {
                if !deps.contains_key("@qontinui/ui-bridge-server") {
                    deps.insert(
                        "@qontinui/ui-bridge-server".to_string(),
                        serde_json::Value::String("latest".to_string()),
                    );
                    let existing_mod = modifications
                        .iter_mut()
                        .find(|m| m.file_path == "package.json");
                    if let Some(existing) = existing_mod {
                        existing.new_content = serde_json::to_string_pretty(&pkg)
                            .unwrap_or_else(|_| existing.new_content.clone());
                    } else {
                        let new_content =
                            serde_json::to_string_pretty(&pkg).unwrap_or_else(|_| content.clone());
                        modifications.push(FileModification {
                            file_path: "package.json".to_string(),
                            modification_type: ModificationType::Replace,
                            description: "Add @qontinui/ui-bridge-server to dependencies"
                                .to_string(),
                            original_content: Some(content),
                            new_content,
                        });
                    }
                }
            }
        }
    }
}

async fn add_babel_plugin_to_package_json(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
) {
    let pkg_path = project.join("package.json");
    if let Ok(content) = tokio::fs::read_to_string(&pkg_path).await {
        if let Ok(mut pkg) = serde_json::from_str::<serde_json::Value>(&content) {
            let dev_deps = pkg.as_object_mut().and_then(|obj| {
                if !obj.contains_key("devDependencies") {
                    obj.insert("devDependencies".to_string(), serde_json::json!({}));
                }
                obj.get_mut("devDependencies")
                    .and_then(|d| d.as_object_mut())
            });
            if let Some(dev_deps) = dev_deps {
                if !dev_deps.contains_key("@qontinui/ui-bridge-babel-plugin") {
                    dev_deps.insert(
                        "@qontinui/ui-bridge-babel-plugin".to_string(),
                        serde_json::Value::String("latest".to_string()),
                    );
                    // Merge with existing package.json modification if one exists
                    let existing_mod = modifications
                        .iter_mut()
                        .find(|m| m.file_path == "package.json");
                    if let Some(existing) = existing_mod {
                        // Re-parse the existing new_content and merge devDependencies
                        if let Ok(mut existing_pkg) =
                            serde_json::from_str::<serde_json::Value>(&existing.new_content)
                        {
                            let existing_dev = existing_pkg.as_object_mut().and_then(|obj| {
                                if !obj.contains_key("devDependencies") {
                                    obj.insert(
                                        "devDependencies".to_string(),
                                        serde_json::json!({}),
                                    );
                                }
                                obj.get_mut("devDependencies")
                                    .and_then(|d| d.as_object_mut())
                            });
                            if let Some(existing_dev) = existing_dev {
                                existing_dev.insert(
                                    "@qontinui/ui-bridge-babel-plugin".to_string(),
                                    serde_json::Value::String("latest".to_string()),
                                );
                            }
                            existing.new_content = serde_json::to_string_pretty(&existing_pkg)
                                .unwrap_or_else(|_| existing.new_content.clone());
                        }
                    } else {
                        let new_content =
                            serde_json::to_string_pretty(&pkg).unwrap_or_else(|_| content.clone());
                        modifications.push(FileModification {
                            file_path: "package.json".to_string(),
                            modification_type: ModificationType::Replace,
                            description: "Add @qontinui/ui-bridge-babel-plugin to devDependencies"
                                .to_string(),
                            original_content: Some(content),
                            new_content,
                        });
                    }
                }
            }
        }
    }
}

async fn add_babel_to_next_config(
    project: &std::path::Path,
    modifications: &mut Vec<FileModification>,
    warnings: &mut Vec<String>,
) {
    // Check for .babelrc first
    let babelrc_path = project.join(".babelrc");
    if babelrc_path.exists() {
        if let Ok(content) = tokio::fs::read_to_string(&babelrc_path).await {
            if !content.contains("@qontinui/ui-bridge-babel-plugin") {
                if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) {
                    let plugins = config.as_object_mut().and_then(|obj| {
                        if !obj.contains_key("plugins") {
                            obj.insert("plugins".to_string(), serde_json::json!([]));
                        }
                        obj.get_mut("plugins").and_then(|p| p.as_array_mut())
                    });
                    if let Some(plugins) = plugins {
                        plugins.push(serde_json::json!("@qontinui/ui-bridge-babel-plugin"));
                        let new_content = serde_json::to_string_pretty(&config)
                            .unwrap_or_else(|_| content.clone());
                        modifications.push(FileModification {
                            file_path: ".babelrc".to_string(),
                            modification_type: ModificationType::Replace,
                            description: "Add UI Bridge babel plugin to .babelrc".to_string(),
                            original_content: Some(content),
                            new_content,
                        });
                    }
                }
            }
        }
        return;
    }

    // For Next.js without .babelrc, create one
    let next_config_exists = project.join("next.config.js").exists()
        || project.join("next.config.mjs").exists()
        || project.join("next.config.ts").exists();

    if next_config_exists {
        modifications.push(FileModification {
            file_path: ".babelrc".to_string(),
            modification_type: ModificationType::CreateNew,
            description: "Create .babelrc with UI Bridge babel plugin for auto-instrumentation"
                .to_string(),
            original_content: None,
            new_content: serde_json::to_string_pretty(&serde_json::json!({
                "presets": ["next/babel"],
                "plugins": ["@qontinui/ui-bridge-babel-plugin"]
            }))
            .unwrap(),
        });
        warnings.push(
            "Created .babelrc — this disables Next.js SWC compiler. Build times may increase."
                .to_string(),
        );
    }
}

fn wrap_with_provider_react(content: &str, file_path: &str) -> String {
    let import_line =
        "import { UIBridgeProvider, AutoRegisterProvider } from '@qontinui/ui-bridge/react';\n";
    let insert_pos = find_last_import_pos(content);

    let is_main = file_path.contains("main.") || file_path.contains("index.");
    if is_main {
        let mut result = String::new();
        result.push_str(&content[..insert_pos]);
        result.push_str(import_line);
        let rest = &content[insert_pos..];
        let wrapped = rest
            .replace(
                "<App />",
                "<UIBridgeProvider>\n      <AutoRegisterProvider>\n        <App />\n      </AutoRegisterProvider>\n    </UIBridgeProvider>",
            )
            .replace(
                "<App/>",
                "<UIBridgeProvider>\n      <AutoRegisterProvider>\n        <App />\n      </AutoRegisterProvider>\n    </UIBridgeProvider>",
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
        "import { UIBridgeProvider, AutoRegisterProvider } from '@qontinui/ui-bridge/react';\n";
    let insert_pos = find_last_import_pos(content);

    let mut result = String::new();
    result.push_str(&content[..insert_pos]);
    result.push_str(import_line);
    let rest = &content[insert_pos..];
    let wrapped = rest.replace(
        "{children}",
        "{/* UI Bridge Integration */}\n          <UIBridgeProvider>\n            <AutoRegisterProvider>\n              {children}\n            </AutoRegisterProvider>\n          </UIBridgeProvider>",
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

const NEXTJS_API_ROUTE_TEMPLATE: &str = r#"import { createUIBridgeHandler } from '@qontinui/ui-bridge-server/nextjs';

const handler = createUIBridgeHandler();

export const GET = handler;
export const POST = handler;
export const PUT = handler;
export const DELETE = handler;
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
             status, proxy_port, target_url, last_health_check, element_count,
             created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"#,
        rusqlite::params![
            integration.id,
            integration.project_path,
            integration.label,
            integration.framework,
            integration.integration_type,
            integration.sdk_version,
            integration.status,
            integration.proxy_port,
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
                      status, proxy_port, target_url, last_health_check, element_count,
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
                proxy_port: row.get(7)?,
                target_url: row.get(8)?,
                last_health_check: row.get(9)?,
                element_count: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
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

async fn handle_inject(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<InjectRequest>,
) -> Json<ApiResponse<InjectResponse>> {
    let proxy_manager = get_proxy_manager();
    let mut manager = proxy_manager.lock().await;
    let port = manager.allocate_port();
    let label = req
        .label
        .unwrap_or_else(|| format!("Proxy for {}", req.target_url));

    match start_proxy(req.target_url.clone(), port).await {
        Ok(shutdown_tx) => {
            let now = chrono::Utc::now().timestamp();
            let instance = ProxyInstance {
                proxy_url: format!("http://localhost:{}", port),
                proxy_port: port,
                target_url: req.target_url.clone(),
                label: label.clone(),
                status: "active".to_string(),
                element_count: None,
                started_at: now,
                shutdown_tx: Some(shutdown_tx),
            };

            let response = InjectResponse {
                proxy_url: instance.proxy_url.clone(),
                proxy_port: port,
                target_url: req.target_url.clone(),
                status: "active".to_string(),
            };

            manager.proxies.insert(port, instance);

            // Track in database
            let integration = TrackedIntegration {
                id: format!("proxy-{}", port),
                project_path: String::new(),
                label: Some(label),
                framework: None,
                integration_type: "runtime".to_string(),
                sdk_version: Some("injected".to_string()),
                status: "active".to_string(),
                proxy_port: Some(port),
                target_url: Some(req.target_url),
                last_health_check: Some(now),
                element_count: None,
                created_at: now,
                updated_at: now,
            };
            let _ = save_integration(&state.app_state.checkpoint_db, &integration);

            // Auto-connect SDK client to the new proxy
            let proxy_url_for_sdk = format!("http://127.0.0.1:{}", port);
            let sdk_conn = state.sdk_connection.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                match crate::mcp::sdk_client::connect_sdk_app(
                    &sdk_conn,
                    &proxy_url_for_sdk,
                    Some(port),
                    None,
                    None,
                    None,
                    None,
                )
                .await
                {
                    Ok(_) => info!("Auto-connected SDK to proxy at {}", proxy_url_for_sdk),
                    Err(e) => warn!("Auto-connect to proxy {} failed: {}", proxy_url_for_sdk, e),
                }
            });

            Json(ApiResponse::success(response))
        }
        Err(e) => Json(ApiResponse::error(e)),
    }
}

async fn handle_list_proxies(
    State(_state): State<Arc<ApiState>>,
) -> Json<ApiResponse<Vec<ProxyInfo>>> {
    let proxy_manager = get_proxy_manager();
    let manager = proxy_manager.lock().await;
    let proxies: Vec<ProxyInfo> = manager
        .proxies
        .values()
        .map(|p| ProxyInfo {
            proxy_url: p.proxy_url.clone(),
            proxy_port: p.proxy_port,
            target_url: p.target_url.clone(),
            label: p.label.clone(),
            status: p.status.clone(),
            element_count: p.element_count,
            started_at: p.started_at,
        })
        .collect();
    Json(ApiResponse::success(proxies))
}

async fn handle_stop_proxy(
    State(state): State<Arc<ApiState>>,
    Path(port): Path<u16>,
) -> Json<ApiResponse<String>> {
    let proxy_manager = get_proxy_manager();
    let mut manager = proxy_manager.lock().await;
    if let Some(mut instance) = manager.proxies.remove(&port) {
        if let Some(tx) = instance.shutdown_tx.take() {
            let _ = tx.send(());
        }
        let _ = delete_integration(&state.app_state.checkpoint_db, &format!("proxy-{}", port));
        info!("Stopped proxy on port {}", port);
        Json(ApiResponse::success(format!(
            "Proxy on port {} stopped",
            port
        )))
    } else {
        Json(ApiResponse::error(format!(
            "No proxy found on port {}",
            port
        )))
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
                    proxy_port: None,
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
        // Determine the URL to check
        let check_url = if integration.integration_type == "runtime" {
            integration
                .proxy_port
                .map(|p| format!("http://localhost:{}/__ui-bridge/health", p))
        } else {
            integration
                .target_url
                .as_ref()
                .map(|u| format!("{}/api/ui-bridge/health", u.trim_end_matches('/')))
        };

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
        .route("/ui-bridge/integration/inject", post(handle_inject))
        .route("/ui-bridge/integration/proxies", get(handle_list_proxies))
        .route(
            "/ui-bridge/integration/inject/{port}",
            delete(handle_stop_proxy),
        )
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
