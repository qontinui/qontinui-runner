//! Discovery Tools for Workflow Generation
//!
//! Runs a heuristic-driven discovery phase before the Builder Agent to gather
//! structured information about the target system. Discovery tools are read-only
//! and execute based on keywords in the user's description — no AI needed.
//!
//! Results are formatted as markdown and injected into the builder prompt as
//! additional context, improving step accuracy (real paths, valid check_types,
//! correct API response shapes, etc.).

use crate::check_generation::{
    recommended_tools_for_language, scan_workspace, ScanWorkspaceRequest,
};
use crate::str_utils::truncate_str;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;
use tracing::{debug, info, warn};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for the discovery phase.
pub struct DiscoveryConfig {
    /// Runner HTTP port (default: 9876)
    pub runner_port: u16,
    /// Timeout per tool in seconds (default: 15)
    pub per_tool_timeout_secs: u64,
    /// Maximum characters per tool result (default: 6000)
    pub max_result_chars: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            runner_port: 9876,
            per_tool_timeout_secs: 15,
            max_result_chars: 6000,
        }
    }
}

// ============================================================================
// Input / Output types
// ============================================================================

/// Structured data extracted from the user's description.
#[derive(Debug, Clone)]
pub struct DiscoveryInput {
    /// Original description text
    pub description: String,
    /// Lowercased description for keyword matching
    pub description_lower: String,
    /// Extracted absolute directory paths
    pub project_paths: Vec<String>,
    /// Extracted URLs (http/https)
    pub target_urls: Vec<String>,
    /// Extracted API endpoint paths (/api/..., /v1/..., etc.)
    pub api_endpoints: Vec<String>,
}

/// Result of the discovery phase.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// Formatted markdown to inject into the builder prompt
    pub context: String,
    /// Which tools ran and their outcomes
    pub calls: Vec<DiscoveryCall>,
}

/// Record of a single discovery tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryCall {
    pub tool_name: String,
    pub input_summary: String,
    pub success: bool,
    pub duration_ms: u64,
}

// ============================================================================
// Tool definitions
// ============================================================================

/// Names of all supported discovery tools.
const TOOL_SCAN_WORKSPACE: &str = "scan_workspace";
const TOOL_LIST_CHECKS: &str = "list_checks";
const TOOL_SCAN_APPS: &str = "scan_apps";
const TOOL_SDK_SNAPSHOT: &str = "sdk_snapshot";
const TOOL_UI_BRIDGE_SPECS: &str = "ui_bridge_specs";
const TOOL_TEST_API: &str = "test_api_request";
const TOOL_LIST_MCP: &str = "list_mcp_servers";
const TOOL_LIST_SHELL_COMMANDS: &str = "list_shell_commands";
const TOOL_DATABASE_STATS: &str = "database_stats";

/// Keywords that trigger each tool.
struct ToolTrigger {
    name: &'static str,
    keywords: &'static [&'static str],
    /// Whether this tool requires specific extracted data (not just keywords)
    needs_data: ToolDataReq,
}

#[derive(PartialEq)]
enum ToolDataReq {
    /// Triggers on keywords alone
    KeywordsOnly,
    /// Requires at least one project_path
    ProjectPath,
    /// Requires at least one target URL or web-related keywords
    WebApp,
    /// Requires an API endpoint or URL + API keywords
    ApiEndpoint,
}

const TOOL_TRIGGERS: &[ToolTrigger] = &[
    ToolTrigger {
        name: TOOL_SCAN_WORKSPACE,
        keywords: &[], // triggers when a project path is detected
        needs_data: ToolDataReq::ProjectPath,
    },
    ToolTrigger {
        name: TOOL_LIST_CHECKS,
        keywords: &[
            "lint",
            "check",
            "typecheck",
            "type check",
            "format",
            "code quality",
            "ruff",
            "eslint",
            "mypy",
            "prettier",
            "black",
            "clippy",
            "test",
            "ci",
            "pre-commit",
        ],
        needs_data: ToolDataReq::KeywordsOnly,
    },
    ToolTrigger {
        name: TOOL_SCAN_APPS,
        keywords: &[
            "web app",
            "localhost",
            "frontend",
            "dashboard",
            "page",
            "ui",
            "browser",
            "website",
            "web page",
            "web application",
            "app",
            "site",
            "portal",
            "interface",
        ],
        needs_data: ToolDataReq::WebApp,
    },
    ToolTrigger {
        name: TOOL_SDK_SNAPSHOT,
        keywords: &[
            "web app",
            "localhost",
            "frontend",
            "dashboard",
            "page",
            "ui",
            "browser",
            "website",
            "element",
            "button",
            "input",
            "form",
        ],
        needs_data: ToolDataReq::WebApp,
    },
    ToolTrigger {
        name: TOOL_UI_BRIDGE_SPECS,
        keywords: &[
            "spec",
            "page spec",
            "verify",
            "verification",
            "test",
            "check",
            "web app",
            "frontend",
            "page",
            "ui",
            "dashboard",
            "app",
            "quality",
            "assert",
            "semantic",
        ],
        needs_data: ToolDataReq::WebApp,
    },
    ToolTrigger {
        name: TOOL_TEST_API,
        keywords: &[
            "api", "endpoint", "request", "response", "rest", "http", "backend", "server",
            "status", "health",
        ],
        needs_data: ToolDataReq::ApiEndpoint,
    },
    ToolTrigger {
        name: TOOL_LIST_MCP,
        keywords: &["mcp", "tool", "server", "mcp server"],
        needs_data: ToolDataReq::KeywordsOnly,
    },
    ToolTrigger {
        name: TOOL_LIST_SHELL_COMMANDS,
        keywords: &["script", "shell", "command", "run", "build", "deploy"],
        needs_data: ToolDataReq::KeywordsOnly,
    },
    ToolTrigger {
        name: TOOL_DATABASE_STATS,
        keywords: &[
            "database",
            "db",
            "data",
            "task run",
            "workflow history",
            "storage",
        ],
        needs_data: ToolDataReq::KeywordsOnly,
    },
];

// ============================================================================
// Description parser
// ============================================================================

/// Parse the user's description to extract paths, URLs, and API endpoints.
pub fn parse_description(description: &str) -> DiscoveryInput {
    let description_lower = description.to_lowercase();

    // Extract absolute paths (Windows and Unix)
    let project_paths = extract_paths(description);

    // Extract URLs
    let target_urls = extract_urls(description);

    // Extract API endpoints
    let api_endpoints = extract_api_endpoints(description);

    debug!(
        "Parsed description: {} paths, {} URLs, {} API endpoints",
        project_paths.len(),
        target_urls.len(),
        api_endpoints.len()
    );

    DiscoveryInput {
        description: description.to_string(),
        description_lower,
        project_paths,
        target_urls,
        api_endpoints,
    }
}

/// Extract absolute file/directory paths from text.
fn extract_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();

    // Windows absolute paths: C:\..., D:\...
    // Match drive letter followed by :\ or :/ then path chars
    let mut i = 0;
    let chars: Vec<char> = text.chars().collect();
    while i < chars.len() {
        if i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && (chars[i + 2] == '\\' || chars[i + 2] == '/')
        {
            // Check it's at start or after whitespace/quote
            if i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '"' || chars[i - 1] == '\''
            {
                let start = i;
                i += 3;
                // Consume path characters
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && chars[i] != '"'
                    && chars[i] != '\''
                    && chars[i] != ')'
                    && chars[i] != ','
                {
                    i += 1;
                }
                let path = chars[start..i].iter().collect::<String>();
                // Strip trailing punctuation
                let path = path.trim_end_matches(['.', ';']).to_string();
                if path.len() > 3 {
                    paths.push(path);
                }
                continue;
            }
        }

        // Unix absolute paths: /home/..., /Users/..., /opt/...
        if chars[i] == '/'
            && (i == 0
                || chars[i - 1].is_whitespace()
                || chars[i - 1] == '"'
                || chars[i - 1] == '\'')
        {
            // Must start with known prefixes to avoid matching API endpoints
            let remaining: String = chars[i..].iter().collect();
            if remaining.starts_with("/home/")
                || remaining.starts_with("/Users/")
                || remaining.starts_with("/opt/")
                || remaining.starts_with("/var/")
                || remaining.starts_with("/tmp/")
                || remaining.starts_with("/srv/")
            {
                let start = i;
                i += 1;
                while i < chars.len()
                    && !chars[i].is_whitespace()
                    && chars[i] != '"'
                    && chars[i] != '\''
                    && chars[i] != ')'
                    && chars[i] != ','
                {
                    i += 1;
                }
                let path = chars[start..i].iter().collect::<String>();
                let path = path.trim_end_matches(['.', ';']).to_string();
                if path.len() > 5 {
                    paths.push(path);
                }
                continue;
            }
        }

        i += 1;
    }

    paths
}

/// Extract HTTP/HTTPS URLs from text.
fn extract_urls(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for word in text.split_whitespace() {
        let word =
            word.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')' || c == ',');
        if word.starts_with("http://") || word.starts_with("https://") {
            // Trim trailing punctuation that's not part of URLs
            let url = word.trim_end_matches(['.', ';', ')']);
            urls.push(url.to_string());
        }
    }
    urls
}

/// Extract API endpoint paths from text.
fn extract_api_endpoints(text: &str) -> Vec<String> {
    let mut endpoints = Vec::new();
    for word in text.split_whitespace() {
        let word =
            word.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')' || c == ',');
        if (word.starts_with("/api/") || word.starts_with("/v1/") || word.starts_with("/v2/"))
            && word.len() > 5
        {
            endpoints.push(word.to_string());
        }
    }
    endpoints
}

// ============================================================================
// Tool selection
// ============================================================================

/// Determine which discovery tools to run based on the parsed description.
///
/// When `mode` is `"enabled"`, all tools whose data requirements are met are
/// selected, regardless of keyword matching. In `"auto"` mode (the default),
/// keyword matching is used to narrow the selection.
pub fn select_tools(input: &DiscoveryInput, mode: &str) -> Vec<&'static str> {
    let enabled_mode = mode == "enabled";
    let mut selected = Vec::new();

    for trigger in TOOL_TRIGGERS {
        let data_ok = match trigger.needs_data {
            ToolDataReq::KeywordsOnly => true,
            ToolDataReq::ProjectPath => !input.project_paths.is_empty(),
            ToolDataReq::WebApp => {
                !input.target_urls.is_empty()
                    || trigger
                        .keywords
                        .iter()
                        .any(|kw| input.description_lower.contains(kw))
            }
            ToolDataReq::ApiEndpoint => {
                !input.api_endpoints.is_empty()
                    || (!input.target_urls.is_empty()
                        && trigger
                            .keywords
                            .iter()
                            .any(|kw| input.description_lower.contains(kw)))
            }
        };

        if !data_ok {
            continue;
        }

        // In "enabled" mode, skip keyword check — only data requirements matter
        if enabled_mode {
            selected.push(trigger.name);
            continue;
        }

        // For auto mode, check keyword match
        let keyword_match = if trigger.keywords.is_empty() {
            // No keywords means the tool triggers purely on data presence
            true
        } else {
            trigger
                .keywords
                .iter()
                .any(|kw| input.description_lower.contains(kw))
        };

        if keyword_match {
            selected.push(trigger.name);
        }
    }

    selected
}

// ============================================================================
// Workspace scan cache
// ============================================================================

/// Cache for workspace scan results, keyed by path. TTL = 5 minutes.
static WORKSPACE_CACHE: Lazy<Mutex<HashMap<String, (Instant, String)>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

const CACHE_TTL_SECS: u64 = 300;

/// Look up a cached workspace scan result. Returns `None` if not found or expired.
fn get_cached_workspace_scan(path: &str) -> Option<String> {
    let cache = WORKSPACE_CACHE.lock().ok()?;
    if let Some((timestamp, result)) = cache.get(path) {
        if timestamp.elapsed().as_secs() < CACHE_TTL_SECS {
            debug!("Workspace cache hit for {}", path);
            return Some(result.clone());
        }
    }
    None
}

/// Store a workspace scan result in the cache.
fn set_cached_workspace_scan(path: &str, result: &str) {
    if let Ok(mut cache) = WORKSPACE_CACHE.lock() {
        cache.insert(path.to_string(), (Instant::now(), result.to_string()));
    }
}

// ============================================================================
// Tool execution
// ============================================================================

/// Run all selected discovery tools and return formatted results.
///
/// `mode` controls tool selection: `"auto"` (keyword-based), `"enabled"` (all
/// tools with satisfied data requirements), or `"disabled"` (caller should
/// skip calling this entirely).
pub fn run_discovery(description: &str, config: &DiscoveryConfig, mode: &str) -> DiscoveryResult {
    let input = parse_description(description);
    let tools = select_tools(&input, mode);

    if tools.is_empty() {
        debug!("No discovery tools selected for this description");
        return DiscoveryResult {
            context: String::new(),
            calls: vec![],
        };
    }

    info!("Running {} discovery tools: {:?}", tools.len(), tools);

    let mut calls = Vec::new();
    let mut sections = Vec::new();

    for tool_name in &tools {
        let start = Instant::now();
        let result = execute_tool(tool_name, &input, config);
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(section) => {
                let truncated = truncate_at_line_boundary(&section, config.max_result_chars);
                calls.push(DiscoveryCall {
                    tool_name: tool_name.to_string(),
                    input_summary: summarize_input(tool_name, &input),
                    success: true,
                    duration_ms,
                });
                if !truncated.is_empty() {
                    sections.push(truncated);
                }
                debug!("{} completed in {}ms", tool_name, duration_ms);
            }
            Err(e) => {
                warn!("{} failed: {}", tool_name, e);
                calls.push(DiscoveryCall {
                    tool_name: tool_name.to_string(),
                    input_summary: summarize_input(tool_name, &input),
                    success: false,
                    duration_ms,
                });
            }
        }
    }

    let context = if sections.is_empty() {
        String::new()
    } else {
        format!(
            "## Discovery Results\n\n\
             The following information was gathered about the target system. \
             Use it to generate accurate steps.\n\n{}",
            sections.join("\n\n")
        )
    };

    info!(
        "Discovery complete: {}/{} tools succeeded, {} chars of context",
        calls.iter().filter(|c| c.success).count(),
        calls.len(),
        context.len()
    );

    DiscoveryResult { context, calls }
}

/// Run a single discovery tool by name and return its formatted markdown section.
///
/// Useful when the caller wants to force a specific tool that wasn't selected
/// by keyword matching (e.g., `discover_ui_bridge_specs: true` in the request).
pub fn run_single_tool(name: &str, description: &str, config: &DiscoveryConfig) -> Option<String> {
    let input = parse_description(description);
    let start = Instant::now();
    match execute_tool(name, &input, config) {
        Ok(section) => {
            let truncated = truncate_at_line_boundary(&section, config.max_result_chars);
            debug!(
                "{} (single) completed in {}ms",
                name,
                start.elapsed().as_millis()
            );
            if truncated.is_empty() {
                None
            } else {
                Some(truncated)
            }
        }
        Err(e) => {
            warn!("{} (single) failed: {}", name, e);
            None
        }
    }
}

/// Execute a single discovery tool and return its markdown section.
fn execute_tool(
    name: &str,
    input: &DiscoveryInput,
    config: &DiscoveryConfig,
) -> Result<String, String> {
    match name {
        TOOL_SCAN_WORKSPACE => execute_scan_workspace(input),
        TOOL_LIST_CHECKS => execute_list_checks(config),
        TOOL_SCAN_APPS => execute_scan_apps(config),
        TOOL_SDK_SNAPSHOT => execute_sdk_snapshot(input, config),
        TOOL_UI_BRIDGE_SPECS => execute_ui_bridge_specs(config),
        TOOL_TEST_API => execute_test_api(input, config),
        TOOL_LIST_MCP => execute_list_mcp_servers(config),
        TOOL_LIST_SHELL_COMMANDS => execute_list_shell_commands(config),
        TOOL_DATABASE_STATS => execute_database_stats(config),
        _ => Err(format!("Unknown tool: {}", name)),
    }
}

/// Summarize what input a tool used (for logging).
fn summarize_input(tool_name: &str, input: &DiscoveryInput) -> String {
    match tool_name {
        TOOL_SCAN_WORKSPACE => {
            if let Some(p) = input.project_paths.first() {
                format!("path: {}", p)
            } else {
                "no path".to_string()
            }
        }
        TOOL_SDK_SNAPSHOT => {
            if let Some(u) = input.target_urls.first() {
                format!("url: {}", u)
            } else {
                "auto-detect".to_string()
            }
        }
        TOOL_TEST_API => {
            let endpoints: Vec<&str> = input.api_endpoints.iter().map(|s| s.as_str()).collect();
            format!("endpoints: {:?}", &endpoints[..endpoints.len().min(3)])
        }
        TOOL_UI_BRIDGE_SPECS => "runner + cached specs".to_string(),
        _ => "default".to_string(),
    }
}

// ============================================================================
// Individual tool implementations
// ============================================================================

/// Scan workspace directories for project structure.
/// Results are cached for 5 minutes to avoid repeated filesystem scans.
fn execute_scan_workspace(input: &DiscoveryInput) -> Result<String, String> {
    let mut sections = Vec::new();

    for path in &input.project_paths {
        // Check cache first
        if let Some(cached) = get_cached_workspace_scan(path) {
            sections.push(cached);
            continue;
        }

        let request = ScanWorkspaceRequest {
            base_directory: path.clone(),
            max_depth: 2,
        };

        match scan_workspace(&request) {
            Ok(result) => {
                let mut s = format!("### Workspace Scan: {}\n", path);
                if result.projects.is_empty() {
                    s.push_str("- No projects detected\n");
                } else {
                    s.push_str(&format!(
                        "- **Projects found:** {}\n",
                        result.projects.len()
                    ));
                    for proj in &result.projects {
                        let langs = proj.languages.join(", ");
                        let configs = proj.config_files.join(", ");
                        s.push_str(&format!(
                            "  - `{}` — {} ({})\n",
                            proj.relative_path,
                            if langs.is_empty() { "unknown" } else { &langs },
                            configs
                        ));
                        // Add recommended tools for each detected language
                        for lang in &proj.languages {
                            let tools = recommended_tools_for_language(lang);
                            if !tools.is_empty() {
                                let tool_list: Vec<String> = tools
                                    .iter()
                                    .map(|t| format!("{} ({})", t.tool, t.check_type))
                                    .collect();
                                s.push_str(&format!(
                                    "    - Recommended checks: {}\n",
                                    tool_list.join(", ")
                                ));
                            }
                        }
                    }
                }
                set_cached_workspace_scan(path, &s);
                sections.push(s);
            }
            Err(e) => {
                warn!("scan_workspace failed for {}: {}", path, e);
            }
        }
    }

    if sections.is_empty() {
        Err("No workspace paths to scan".to_string())
    } else {
        Ok(sections.join("\n"))
    }
}

/// List existing code quality checks from the runner.
fn execute_list_checks(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let url = format!("http://localhost:{}/checks", config.runner_port);
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GET /checks returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let mut s = String::from("### Existing Checks\n");

    // The response is typically { "data": [...] } or just [...]
    let checks = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        data
    } else if let Some(arr) = body.as_array() {
        arr
    } else {
        return Ok(s + "- No checks found\n");
    };

    if checks.is_empty() {
        s.push_str("- No checks configured\n");
    } else {
        for check in checks {
            let name = check
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            let check_type = check
                .get("check_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let command = check.get("command").and_then(|v| v.as_str()).unwrap_or("");
            s.push_str(&format!("- **{}** (type: `{}`)", name, check_type));
            if !command.is_empty() {
                s.push_str(&format!(", command: `{}`", command));
            }
            s.push('\n');
        }
    }

    Ok(s)
}

/// Scan for running UI Bridge-enabled applications.
fn execute_scan_apps(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let url = format!(
        "http://localhost:{}/ui-bridge/apps/scan",
        config.runner_port
    );
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!(
            "GET /ui-bridge/apps/scan returned {}",
            resp.status()
        ));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let mut s = String::from("### Running Apps\n");

    let apps = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        data
    } else if let Some(arr) = body.as_array() {
        arr
    } else {
        return Ok(s + "- No apps found\n");
    };

    if apps.is_empty() {
        s.push_str("- No running apps detected\n");
    } else {
        for app in apps {
            let name = app
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let url_val = app.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let app_type = app.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let sdk = app
                .get("sdk_available")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            s.push_str(&format!("- **{}**", name));
            if !url_val.is_empty() {
                s.push_str(&format!(" at {}", url_val));
            }
            if !app_type.is_empty() {
                s.push_str(&format!(" ({})", app_type));
            }
            if sdk {
                s.push_str(" — UI Bridge SDK available");
            }
            s.push('\n');
        }
    }

    Ok(s)
}

/// Connect to an app's SDK and take a snapshot of the current page.
fn execute_sdk_snapshot(
    input: &DiscoveryInput,
    config: &DiscoveryConfig,
) -> Result<String, String> {
    let client = build_http_client(config)?;
    let base = format!("http://localhost:{}", config.runner_port);

    // Determine the target URL — prefer explicitly mentioned URLs, otherwise try to auto-detect
    let target_url = if let Some(url) = input.target_urls.first() {
        url.clone()
    } else {
        // Check if an app is already connected
        let status_url = format!("{}/ui-bridge/sdk/status", base);
        let status_resp = client
            .get(&status_url)
            .send()
            .map_err(|e| format!("HTTP error: {}", e))?;
        if status_resp.status().is_success() {
            let status: serde_json::Value = status_resp.json().unwrap_or_default();
            let connected = status
                .get("data")
                .and_then(|d| d.get("connected"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if connected {
                // Already connected, go straight to snapshot
                return take_sdk_snapshot(&client, &base, config);
            }
        }
        return Err("No target URL detected and no SDK connection active".to_string());
    };

    // Connect to the app
    let connect_url = format!("{}/ui-bridge/sdk/connect", base);
    let connect_body = serde_json::json!({ "url": target_url });
    let connect_resp = client
        .post(&connect_url)
        .json(&connect_body)
        .send()
        .map_err(|e| format!("SDK connect error: {}", e))?;

    if !connect_resp.status().is_success() {
        return Err(format!(
            "SDK connect to {} failed with status {}",
            target_url,
            connect_resp.status()
        ));
    }

    take_sdk_snapshot(&client, &base, config)
}

/// Take an SDK snapshot and format it as markdown.
fn take_sdk_snapshot(
    client: &reqwest::blocking::Client,
    base: &str,
    _config: &DiscoveryConfig,
) -> Result<String, String> {
    let snapshot_url = format!("{}/ui-bridge/sdk/snapshot", base);
    let resp = client
        .get(&snapshot_url)
        .send()
        .map_err(|e| format!("Snapshot error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("SDK snapshot returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let mut s = String::from("### UI Bridge Snapshot\n");

    // Extract useful summary info from the snapshot
    let data = body.get("data").unwrap_or(&body);

    if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
        s.push_str(&format!("- **Page title:** {}\n", title));
    }
    if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
        s.push_str(&format!("- **URL:** {}\n", url));
    }

    // Summarize elements
    if let Some(elements) = data.get("elements").and_then(|v| v.as_array()) {
        let interactive: Vec<&serde_json::Value> = elements
            .iter()
            .filter(|e| {
                let tag = e.get("tag").and_then(|v| v.as_str()).unwrap_or("");
                matches!(tag, "button" | "input" | "select" | "textarea" | "a")
                    || e.get("role").and_then(|v| v.as_str()).is_some_and(|r| {
                        matches!(
                            r,
                            "button" | "link" | "textbox" | "combobox" | "checkbox" | "radio"
                        )
                    })
            })
            .collect();

        let content_count = elements.len() - interactive.len();
        s.push_str(&format!(
            "- **Elements:** {} interactive, {} content\n",
            interactive.len(),
            content_count
        ));

        // List key interactive elements (limited)
        if !interactive.is_empty() {
            s.push_str("- **Key interactive elements:**\n");
            for elem in interactive.iter().take(20) {
                let tag = elem.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
                let id = elem.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let text = elem.get("text").and_then(|v| v.as_str()).unwrap_or("");
                let role = elem.get("role").and_then(|v| v.as_str()).unwrap_or("");

                let mut desc = format!("  - `{}`", tag);
                if !id.is_empty() {
                    desc.push_str(&format!("#`{}`", id));
                }
                if !role.is_empty() && role != tag {
                    desc.push_str(&format!(" [role={}]", role));
                }
                if !text.is_empty() {
                    let truncated_text = if text.len() > 50 {
                        format!("{}...", truncate_str(text, 47))
                    } else {
                        text.to_string()
                    };
                    desc.push_str(&format!(" \"{}\"", truncated_text));
                }
                desc.push('\n');
                s.push_str(&desc);
            }
            if interactive.len() > 20 {
                s.push_str(&format!("  - ... and {} more\n", interactive.len() - 20));
            }
        }
    }

    Ok(s)
}

/// Fetch UI Bridge page specs and format them as project context.
///
/// Queries both cached external app specs and the runner's own bundled specs.
/// Extracts page descriptions (semantic page specs), group summaries, and
/// architecture information to give the workflow generator a deeper
/// understanding of the target application's pages and purpose.
fn execute_ui_bridge_specs(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let base = format!("http://localhost:{}", config.runner_port);

    let mut all_specs: Vec<serde_json::Value> = Vec::new();

    // 1. Fetch cached external app specs
    let cached_url = format!("{}/ui-bridge/sdk/cached-specs", base);
    match client.get(&cached_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let specs = body
                    .get("data")
                    .and_then(|d| d.as_array())
                    .or_else(|| body.as_array());
                if let Some(arr) = specs {
                    for spec in arr {
                        all_specs.push(spec.clone());
                    }
                }
            }
        }
        _ => {
            debug!("No cached external app specs available");
        }
    }

    // 2. Fetch runner's own page specs
    let control_url = format!("{}/ui-bridge/control/specs", base);
    match client.get(&control_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let specs = body
                    .get("data")
                    .and_then(|d| d.as_array())
                    .or_else(|| body.as_array());
                if let Some(arr) = specs {
                    for spec in arr {
                        all_specs.push(spec.clone());
                    }
                }
            }
        }
        _ => {
            debug!("No runner page specs available");
        }
    }

    if all_specs.is_empty() {
        return Err("No UI Bridge page specs found".to_string());
    }

    let header = "### UI Bridge Page Specs\n\n";
    let mut s = String::from(header);
    s.push_str(
        "The following page specifications describe the purpose, structure, and expected behavior \
         of pages in the target application. Use these to understand what each page does and to \
         generate accurate verification steps.\n\n",
    );

    let mut page_count = 0;

    for spec in &all_specs {
        // Each spec can be a CachedAppSpec (with spec_json field) or a direct spec config
        let config_val = if let Some(spec_json_str) = spec.get("spec_json").and_then(|v| v.as_str())
        {
            // CachedAppSpec: parse the spec_json string
            serde_json::from_str::<serde_json::Value>(spec_json_str).ok()
        } else if spec.get("version").is_some() {
            // Direct spec config object
            Some(spec.clone())
        } else {
            // DiscoveredSpec format: { specId, config, appName }
            spec.get("config").cloned()
        };

        let config_val = match config_val {
            Some(v) => v,
            None => continue,
        };

        // Extract spec identity
        let spec_id = spec
            .get("spec_id")
            .or_else(|| spec.get("specId"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let app_name = spec
            .get("app_name")
            .or_else(|| spec.get("appName"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Top-level description = semantic page spec (page purpose)
        let description = config_val
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let page_url = config_val
            .get("metadata")
            .and_then(|m| m.get("pageUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let component = config_val
            .get("metadata")
            .and_then(|m| m.get("component"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let groups = config_val.get("groups").and_then(|v| v.as_array());

        // Skip specs with no meaningful content
        if description.is_empty() && groups.is_none_or(|g| g.is_empty()) {
            continue;
        }

        page_count += 1;

        // Page header
        let title = if !page_url.is_empty() {
            page_url.to_string()
        } else {
            spec_id.to_string()
        };
        s.push_str(&format!("#### {}", title));
        if !app_name.is_empty() {
            s.push_str(&format!(" ({})", app_name));
        }
        s.push('\n');

        if !component.is_empty() {
            s.push_str(&format!("- **Component:** {}\n", component));
        }

        // Semantic page description — the most valuable part for understanding page purpose
        if !description.is_empty() {
            // Truncate very long descriptions to keep context manageable
            let desc = if description.len() > 500 {
                format!("{}...", truncate_str(description, 497))
            } else {
                description.to_string()
            };
            s.push_str(&format!("- **Purpose:** {}\n", desc));
        }

        // Group summaries — show what the page covers without individual assertions
        if let Some(groups) = groups {
            if !groups.is_empty() {
                let total_assertions: usize = groups
                    .iter()
                    .filter_map(|g| g.get("assertions").and_then(|a| a.as_array()))
                    .map(|a| a.len())
                    .sum();

                s.push_str(&format!(
                    "- **Spec coverage:** {} groups, {} assertions\n",
                    groups.len(),
                    total_assertions
                ));

                // List group names with categories for quick overview
                s.push_str("- **Spec groups:**\n");
                for group in groups.iter().take(15) {
                    let group_name = group
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unnamed");
                    let category = group.get("category").and_then(|v| v.as_str()).unwrap_or("");
                    let group_desc = group
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    s.push_str(&format!("  - **{}**", group_name));
                    if !category.is_empty() {
                        s.push_str(&format!(" [{}]", category));
                    }

                    // Include group description (truncated) for semantic context
                    if !group_desc.is_empty() {
                        let desc = if group_desc.len() > 200 {
                            format!("{}...", truncate_str(group_desc, 197))
                        } else {
                            group_desc.to_string()
                        };
                        s.push_str(&format!(": {}", desc));
                    }
                    s.push('\n');
                }
                if groups.len() > 15 {
                    s.push_str(&format!("  - ... and {} more groups\n", groups.len() - 15));
                }
            }
        }

        s.push('\n');
    }

    if page_count == 0 {
        return Err("No meaningful page specs found".to_string());
    }

    // Summary line at the top
    let summary = format!(
        "**{} page specs available.** These semantic page specs describe what each page does, \
         what users can accomplish, and what correct behavior looks like. Use them to generate \
         verification steps that check real page behavior rather than guessing at element names.\n\n",
        page_count
    );
    s.insert_str(header.len(), &summary);

    Ok(s)
}

/// Test an API request and show the response shape.
fn execute_test_api(input: &DiscoveryInput, config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let base = format!("http://localhost:{}", config.runner_port);

    let mut sections = Vec::new();

    // Build test requests from extracted URLs + endpoints
    let mut test_urls: Vec<String> = Vec::new();

    // Direct URLs that look like API endpoints
    for url in &input.target_urls {
        if url.contains("/api/") || url.contains("/v1/") || url.contains("/v2/") {
            test_urls.push(url.clone());
        }
    }

    // Combine base URLs with extracted endpoint paths
    for endpoint in &input.api_endpoints {
        for url in &input.target_urls {
            // Use the URL's origin as base
            if let Some(origin) = extract_origin(url) {
                test_urls.push(format!("{}{}", origin, endpoint));
            }
        }
        // Also try common localhost ports if no URLs specified
        if input.target_urls.is_empty() {
            test_urls.push(format!("http://localhost:8000{}", endpoint));
        }
    }

    // Deduplicate
    test_urls.sort();
    test_urls.dedup();

    // Limit to 3 test requests
    for test_url in test_urls.iter().take(3) {
        let api_test_url = format!("{}/api-request/test", base);
        let body = serde_json::json!({
            "method": "GET",
            "url": test_url,
        });

        match client.post(&api_test_url).json(&body).send() {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(result) = resp.json::<serde_json::Value>() {
                    let mut s = format!("### API Test: GET {}\n", test_url);
                    let data = result.get("data").unwrap_or(&result);

                    if let Some(status) = data.get("status_code").and_then(|v| v.as_u64()) {
                        s.push_str(&format!("- **Status:** {}\n", status));
                    }

                    // Show response body shape (truncated)
                    if let Some(resp_body) = data.get("body") {
                        let pretty = serde_json::to_string_pretty(resp_body)
                            .unwrap_or_else(|_| resp_body.to_string());
                        let truncated = truncate_at_line_boundary(&pretty, 1500);
                        s.push_str(&format!(
                            "- **Response body:**\n```json\n{}\n```\n",
                            truncated
                        ));
                    }

                    sections.push(s);
                }
            }
            Ok(resp) => {
                sections.push(format!(
                    "### API Test: GET {}\n- **Error:** HTTP {}\n",
                    test_url,
                    resp.status()
                ));
            }
            Err(e) => {
                debug!("API test for {} failed: {}", test_url, e);
            }
        }
    }

    if sections.is_empty() {
        Err("No API requests could be tested".to_string())
    } else {
        Ok(sections.join("\n"))
    }
}

/// List available MCP servers and their tools.
fn execute_list_mcp_servers(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let url = format!("http://localhost:{}/mcp-servers", config.runner_port);
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GET /mcp-servers returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let mut s = String::from("### MCP Servers\n");

    let servers = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        data
    } else if let Some(arr) = body.as_array() {
        arr
    } else {
        return Ok(s + "- No MCP servers found\n");
    };

    if servers.is_empty() {
        s.push_str("- No MCP servers configured\n");
    } else {
        for server in servers {
            let name = server
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            s.push_str(&format!("- **{}**", name));

            if let Some(tools) = server.get("tools").and_then(|v| v.as_array()) {
                if !tools.is_empty() {
                    let tool_names: Vec<&str> = tools
                        .iter()
                        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
                        .take(10)
                        .collect();
                    s.push_str(&format!(": {}", tool_names.join(", ")));
                    if tools.len() > 10 {
                        s.push_str(&format!(" (+{} more)", tools.len() - 10));
                    }
                }
            }
            s.push('\n');
        }
    }

    Ok(s)
}

/// List saved shell commands from the runner.
fn execute_list_shell_commands(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let url = format!("http://localhost:{}/shell-commands", config.runner_port);
    let resp = client
        .get(&url)
        .send()
        .map_err(|e| format!("HTTP error: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("GET /shell-commands returned {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("JSON parse error: {}", e))?;

    let mut s = String::from("### Shell Commands\n");

    let commands = if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        data
    } else if let Some(arr) = body.as_array() {
        arr
    } else {
        return Ok(s + "- No shell commands found\n");
    };

    if commands.is_empty() {
        s.push_str("- No saved shell commands\n");
    } else {
        for cmd in commands {
            let name = cmd
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            let description = cmd
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let command = cmd.get("command").and_then(|v| v.as_str()).unwrap_or("");
            s.push_str(&format!("- **{}**", name));
            if !description.is_empty() {
                s.push_str(&format!(": {}", description));
            }
            if !command.is_empty() {
                s.push_str(&format!(" — `{}`", command));
            }
            s.push('\n');
        }
    }

    Ok(s)
}

/// Gather database stats (workflow count, active task runs) from existing endpoints.
fn execute_database_stats(config: &DiscoveryConfig) -> Result<String, String> {
    let client = build_http_client(config)?;
    let base = format!("http://localhost:{}", config.runner_port);

    let mut s = String::from("### Database Stats\n");

    // Get workflow count
    let workflows_url = format!("{}/unified-workflows", base);
    match client.get(&workflows_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let workflows = body
                    .get("data")
                    .and_then(|d| d.as_array())
                    .or_else(|| body.as_array());
                if let Some(wf_list) = workflows {
                    s.push_str(&format!("- **Saved workflows:** {}\n", wf_list.len()));
                }
            }
        }
        Ok(resp) => {
            debug!("GET /unified-workflows returned {}", resp.status());
        }
        Err(e) => {
            debug!("GET /unified-workflows failed: {}", e);
        }
    }

    // Get running task runs
    let running_url = format!("{}/task-runs/running", base);
    match client.get(&running_url).send() {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(body) = resp.json::<serde_json::Value>() {
                let runs = body
                    .get("data")
                    .and_then(|d| d.as_array())
                    .or_else(|| body.as_array());
                if let Some(run_list) = runs {
                    s.push_str(&format!("- **Active task runs:** {}\n", run_list.len()));
                }
            }
        }
        Ok(resp) => {
            debug!("GET /task-runs/running returned {}", resp.status());
        }
        Err(e) => {
            debug!("GET /task-runs/running failed: {}", e);
        }
    }

    // If we didn't get any data, report it
    if s == "### Database Stats\n" {
        return Err("Could not retrieve database stats".to_string());
    }

    Ok(s)
}

// ============================================================================
// Helpers
// ============================================================================

/// Build a blocking HTTP client with timeout.
fn build_http_client(config: &DiscoveryConfig) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(config.per_tool_timeout_secs))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))
}

/// Extract the origin (scheme + host + port) from a URL.
fn extract_origin(url: &str) -> Option<String> {
    // Simple extraction: find the third slash or end
    if let Some(scheme_end) = url.find("://") {
        let rest = &url[scheme_end + 3..];
        if let Some(path_start) = rest.find('/') {
            Some(url[..scheme_end + 3 + path_start].to_string())
        } else {
            Some(url.to_string())
        }
    } else {
        None
    }
}

/// Truncate text at a line boundary, keeping it under `max_chars`.
fn truncate_at_line_boundary(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }

    // Find the last newline before max_chars
    let truncation_point = text[..max_chars].rfind('\n').unwrap_or(max_chars);

    format!("{}...\n(truncated)", &text[..truncation_point])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_description_windows_path() {
        let desc =
            r"Run lint checks on C:\Users\jspin\Documents\qontinui_parent\qontinui-runner project";
        let input = parse_description(desc);
        assert_eq!(input.project_paths.len(), 1);
        assert!(input.project_paths[0].contains("qontinui-runner"));
    }

    #[test]
    fn test_parse_description_unix_path() {
        let desc = "Check the project at /home/user/projects/myapp for type errors";
        let input = parse_description(desc);
        assert_eq!(input.project_paths.len(), 1);
        assert!(input.project_paths[0].contains("myapp"));
    }

    #[test]
    fn test_parse_description_urls() {
        let desc = "Test the API at http://localhost:8000/api/v1/users and verify the frontend at http://localhost:3001";
        let input = parse_description(desc);
        assert_eq!(input.target_urls.len(), 2);
        assert!(input.target_urls.iter().any(|u| u.contains("8000")));
        assert!(input.target_urls.iter().any(|u| u.contains("3001")));
    }

    #[test]
    fn test_parse_description_api_endpoints() {
        let desc =
            "Verify that /api/v1/users returns the correct data and /api/v1/auth/login works";
        let input = parse_description(desc);
        assert_eq!(input.api_endpoints.len(), 2);
    }

    #[test]
    fn test_parse_description_mixed() {
        let desc =
            r"Run linting on C:\Users\jspin\project and test http://localhost:3001/api/v1/health";
        let input = parse_description(desc);
        assert_eq!(input.project_paths.len(), 1);
        assert_eq!(input.target_urls.len(), 1);
    }

    #[test]
    fn test_parse_description_no_data() {
        let desc = "Create a workflow that monitors system health";
        let input = parse_description(desc);
        assert!(input.project_paths.is_empty());
        assert!(input.target_urls.is_empty());
        assert!(input.api_endpoints.is_empty());
    }

    #[test]
    fn test_select_tools_workspace_scan() {
        let input = DiscoveryInput {
            description: "Check the project".to_string(),
            description_lower: "check the project".to_string(),
            project_paths: vec![r"C:\Users\jspin\project".to_string()],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_SCAN_WORKSPACE));
        assert!(!tools.contains(&TOOL_SCAN_APPS));
    }

    #[test]
    fn test_select_tools_lint_keywords() {
        let input = DiscoveryInput {
            description: "Run lint and typecheck".to_string(),
            description_lower: "run lint and typecheck".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_LIST_CHECKS));
    }

    #[test]
    fn test_select_tools_web_app() {
        let input = DiscoveryInput {
            description: "Test the frontend dashboard".to_string(),
            description_lower: "test the frontend dashboard".to_string(),
            project_paths: vec![],
            target_urls: vec!["http://localhost:3001".to_string()],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_SCAN_APPS));
        assert!(tools.contains(&TOOL_SDK_SNAPSHOT));
        assert!(tools.contains(&TOOL_UI_BRIDGE_SPECS));
    }

    #[test]
    fn test_select_tools_api() {
        let input = DiscoveryInput {
            description: "Test the API endpoint".to_string(),
            description_lower: "test the api endpoint".to_string(),
            project_paths: vec![],
            target_urls: vec!["http://localhost:8000".to_string()],
            api_endpoints: vec!["/api/v1/users".to_string()],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_TEST_API));
    }

    #[test]
    fn test_select_tools_mcp() {
        let input = DiscoveryInput {
            description: "Use MCP tools to automate the task".to_string(),
            description_lower: "use mcp tools to automate the task".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_LIST_MCP));
    }

    #[test]
    fn test_select_tools_shell_commands() {
        let input = DiscoveryInput {
            description: "Run the build script for deployment".to_string(),
            description_lower: "run the build script for deployment".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_LIST_SHELL_COMMANDS));
    }

    #[test]
    fn test_select_tools_database_stats() {
        let input = DiscoveryInput {
            description: "Check the database for recent task runs".to_string(),
            description_lower: "check the database for recent task runs".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.contains(&TOOL_DATABASE_STATS));
    }

    #[test]
    fn test_select_tools_none() {
        let input = DiscoveryInput {
            description: "Create a simple timer workflow".to_string(),
            description_lower: "create a simple timer workflow".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "auto");
        assert!(tools.is_empty());
    }

    #[test]
    fn test_select_tools_enabled_mode() {
        // In "enabled" mode, all KeywordsOnly tools should be selected regardless of keywords
        let input = DiscoveryInput {
            description: "Create a simple timer workflow".to_string(),
            description_lower: "create a simple timer workflow".to_string(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "enabled");
        // All KeywordsOnly tools should be included
        assert!(tools.contains(&TOOL_LIST_CHECKS));
        assert!(tools.contains(&TOOL_LIST_MCP));
        assert!(tools.contains(&TOOL_LIST_SHELL_COMMANDS));
        assert!(tools.contains(&TOOL_DATABASE_STATS));
        // Data-dependent tools should NOT be included (no paths/urls/endpoints)
        assert!(!tools.contains(&TOOL_SCAN_WORKSPACE));
    }

    #[test]
    fn test_select_tools_enabled_mode_with_data() {
        let input = DiscoveryInput {
            description: "Something".to_string(),
            description_lower: "something".to_string(),
            project_paths: vec![r"C:\project".to_string()],
            target_urls: vec!["http://localhost:3001".to_string()],
            api_endpoints: vec![],
        };
        let tools = select_tools(&input, "enabled");
        // Should include data-dependent tools when data is present
        assert!(tools.contains(&TOOL_SCAN_WORKSPACE));
        assert!(tools.contains(&TOOL_SCAN_APPS));
        assert!(tools.contains(&TOOL_SDK_SNAPSHOT));
        assert!(tools.contains(&TOOL_UI_BRIDGE_SPECS));
    }

    #[test]
    fn test_truncate_at_line_boundary() {
        let text = "line 1\nline 2\nline 3\nline 4\n";
        let result = truncate_at_line_boundary(text, 15);
        assert!(result.contains("line 1\nline 2"));
        assert!(result.contains("(truncated)"));
    }

    #[test]
    fn test_truncate_short_text() {
        let text = "short";
        let result = truncate_at_line_boundary(text, 100);
        assert_eq!(result, "short");
    }

    #[test]
    fn test_extract_origin() {
        assert_eq!(
            extract_origin("http://localhost:3001/api/v1/users"),
            Some("http://localhost:3001".to_string())
        );
        assert_eq!(
            extract_origin("https://example.com/path"),
            Some("https://example.com".to_string())
        );
        assert_eq!(
            extract_origin("http://localhost:8000"),
            Some("http://localhost:8000".to_string())
        );
        assert_eq!(extract_origin("not-a-url"), None);
    }

    #[test]
    fn test_extract_urls() {
        let urls = extract_urls("Visit http://localhost:3001 and https://api.example.com/v1");
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn test_extract_api_endpoints() {
        let endpoints = extract_api_endpoints("Check /api/v1/users and /v2/health endpoints");
        assert_eq!(endpoints.len(), 2);
    }

    #[test]
    fn test_execute_tool_unknown() {
        let input = DiscoveryInput {
            description: String::new(),
            description_lower: String::new(),
            project_paths: vec![],
            target_urls: vec![],
            api_endpoints: vec![],
        };
        let config = DiscoveryConfig::default();
        let result = execute_tool("nonexistent_tool", &input, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown tool"));
    }

    #[test]
    fn test_format_discovery_results_empty() {
        let result = run_discovery(
            "create a simple workflow",
            &DiscoveryConfig::default(),
            "auto",
        );
        assert!(result.context.is_empty());
        assert!(result.calls.is_empty());
    }
}
