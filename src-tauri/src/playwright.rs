//! Playwright Script Library
//!
//! This module provides persistent storage for Playwright test scripts that users can
//! save, organize, and run from the runner UI.
//!
//! Features:
//! - Store natural language descriptions alongside full .spec.ts code
//! - Execute tests via Node.js subprocess with JSON reporter
//! - Optional cloud sync with qontinui-web

use crate::executor::file_logger::FileLogger;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;

const SCRIPTS_FILE: &str = "playwright-scripts.json";

// ============================================================================
// Data Types
// ============================================================================

/// Sync status for cloud backup
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    #[default]
    LocalOnly,
    Synced,
    LocalModified,
    CloudModified,
    Conflict,
}

/// Display mode for Playwright test execution
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// Run without browser UI (fastest, for CI)
    #[default]
    Headless,
    /// Launch a new browser window with UI visible
    Headed,
    /// Connect to an existing Chrome browser via CDP (Chrome DevTools Protocol)
    ConnectExisting,
}

/// A single test spec result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSpec {
    /// Test title
    pub title: String,
    /// Source file
    pub file: String,
    /// Test status: passed, failed, skipped
    pub status: String,
    /// Duration in milliseconds
    pub duration_ms: u64,
    /// Error message if failed
    #[serde(default)]
    pub error: Option<String>,
    /// Retry count
    #[serde(default)]
    pub retry: u32,
}

/// Structured output for AI analysis
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StructuredTestOutput {
    /// Individual test results
    #[serde(default)]
    pub specs: Vec<TestSpec>,
    /// Console output lines
    #[serde(default)]
    pub console_output: Vec<String>,
    /// Network requests (optional)
    #[serde(default)]
    pub network_requests: Option<Vec<NetworkRequest>>,
    /// Page snapshot from error-context.md (YAML showing all elements on page)
    #[serde(default)]
    pub page_snapshot: Option<String>,
}

/// Network request info (optional tracing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkRequest {
    pub url: String,
    pub method: String,
    pub status: u16,
    pub duration_ms: u64,
}

/// Result of verifying a single success criterion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriteriaResult {
    /// The criterion text
    pub criterion: String,
    /// Whether it was verified as met
    pub verified: bool,
    /// Evidence supporting the verification
    #[serde(default)]
    pub evidence: Option<String>,
}

/// Workflow verification status - tracks whether the workflow objective was achieved
/// (separate from whether the script executed successfully)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowStatus {
    /// The workflow objective (copied from script for reference)
    #[serde(default)]
    pub objective: Option<String>,

    /// Did the script execute without errors? (For automation, expected to be true)
    #[serde(default)]
    pub script_passed: bool,

    /// Explanatory note about script_passed for AI consumption
    #[serde(default)]
    pub script_passed_note: Option<String>,

    /// Was the workflow objective verified as successful?
    /// - None = not yet verified (pending)
    /// - Some(true) = objective confirmed achieved
    /// - Some(false) = objective confirmed NOT achieved
    #[serde(default)]
    pub objective_verified: Option<bool>,

    /// How was the objective verified?
    #[serde(default)]
    pub verification_method: Option<String>, // "automatic", "ai_analysis", "manual", "pending"

    /// Notes from verification
    #[serde(default)]
    pub verification_notes: Option<String>,

    /// Success criteria from the script
    #[serde(default)]
    pub success_criteria: Vec<String>,

    /// Which criteria were confirmed (if verification was performed)
    #[serde(default)]
    pub criteria_results: Vec<CriteriaResult>,

    /// Hints for manual/AI verification
    #[serde(default)]
    pub verification_hints: Vec<String>,
}

/// Result of a Playwright test execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightResult {
    /// Whether all tests passed
    pub passed: bool,
    /// Number of tests that passed
    pub tests_passed: u32,
    /// Number of tests that failed
    pub tests_failed: u32,
    /// Number of tests skipped
    #[serde(default)]
    pub tests_skipped: u32,
    /// Total duration in milliseconds
    pub duration_ms: u64,
    /// Error message if execution failed
    #[serde(default)]
    pub error: Option<String>,
    /// Path to HTML report
    #[serde(default)]
    pub report_path: Option<String>,
    /// Paths to screenshots taken
    #[serde(default)]
    pub screenshots: Vec<String>,
    /// Paths to video recordings
    #[serde(default)]
    pub video_paths: Vec<String>,
    /// Path to trace file (if enabled)
    #[serde(default)]
    pub trace_path: Option<String>,
    /// ISO 8601 timestamp of execution
    pub executed_at: String,
    /// Structured output for AI analysis
    #[serde(default)]
    pub structured_output: Option<StructuredTestOutput>,
    /// Workflow verification status (for AI workflows)
    /// Separates "script passed" from "workflow objective achieved"
    #[serde(default)]
    pub workflow_status: Option<WorkflowStatus>,
}

/// A saved Playwright script
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightScript {
    /// Local unique identifier (UUID v4)
    pub id: String,
    /// Cloud ID (set after first sync)
    #[serde(default)]
    pub cloud_id: Option<String>,
    /// Display name for the script
    pub name: String,
    /// Natural language description of what this test does
    #[serde(default)]
    pub description: String,
    /// Additional instructions for AI code generation/refinement (not part of the test description)
    #[serde(default)]
    pub ai_instructions: Option<String>,
    /// Target web application URL (base URL for tests)
    #[serde(default)]
    pub target_url: String,
    /// The complete .spec.ts file content
    pub script_content: String,
    /// Category for organization (e.g., "E2E", "Smoke", "Regression")
    #[serde(default)]
    pub category: String,
    /// Tags for filtering/searching
    #[serde(default)]
    pub tags: Vec<String>,
    /// Timeout for test execution in seconds
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u32,
    /// Display mode: headless, headed, or connect_existing
    #[serde(default)]
    pub display_mode: DisplayMode,
    /// Browser to use: chromium, firefox, webkit
    #[serde(default = "default_browser")]
    pub browser: String,
    /// Sync status for cloud backup
    #[serde(default)]
    pub sync_status: SyncStatus,
    /// Cloud version (for conflict detection)
    #[serde(default)]
    pub cloud_version: Option<u32>,
    /// Last sync timestamp (ISO 8601)
    #[serde(default)]
    pub last_synced_at: Option<String>,
    /// ISO 8601 timestamp of creation
    pub created_at: String,
    /// ISO 8601 timestamp of last modification
    pub modified_at: String,
    /// Last execution result (cached)
    #[serde(default)]
    pub last_result: Option<PlaywrightResult>,
    /// Workflow objective - what this script aims to accomplish
    /// Used for AI to verify success beyond just "test passed"
    #[serde(default)]
    pub workflow_objective: Option<String>,
    /// Success criteria - specific things to verify after script passes
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// Whether this script is used as workflow automation (not traditional testing)
    /// Automation scripts are expected to pass; verification is what matters
    #[serde(default)]
    pub is_workflow_automation: bool,
}

fn default_timeout_seconds() -> u32 {
    60
}

fn default_browser() -> String {
    "chromium".to_string()
}

impl PlaywrightScript {
    /// Create a new script with auto-generated ID and timestamps
    #[allow(dead_code)]
    pub fn new(name: String, script_content: String) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            cloud_id: None,
            name,
            description: String::new(),
            ai_instructions: None,
            target_url: String::new(),
            script_content,
            category: String::new(),
            tags: Vec::new(),
            timeout_seconds: default_timeout_seconds(),
            display_mode: DisplayMode::default(),
            browser: default_browser(),
            sync_status: SyncStatus::default(),
            cloud_version: None,
            last_synced_at: None,
            created_at: now.clone(),
            modified_at: now,
            last_result: None,
            workflow_objective: None,
            success_criteria: Vec::new(),
            is_workflow_automation: false,
        }
    }

    /// Create a new script with all fields specified
    #[allow(clippy::too_many_arguments)]
    pub fn with_details(
        name: String,
        description: String,
        ai_instructions: Option<String>,
        target_url: String,
        script_content: String,
        category: String,
        tags: Vec<String>,
        timeout_seconds: u32,
        display_mode: DisplayMode,
        browser: String,
    ) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4().to_string(),
            cloud_id: None,
            name,
            description,
            ai_instructions,
            target_url,
            script_content,
            category,
            tags,
            timeout_seconds,
            display_mode,
            browser,
            sync_status: SyncStatus::default(),
            cloud_version: None,
            last_synced_at: None,
            created_at: now.clone(),
            modified_at: now,
            last_result: None,
            workflow_objective: None,
            success_criteria: Vec::new(),
            is_workflow_automation: false,
        }
    }
}

/// The script library containing all saved scripts
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaywrightLibrary {
    /// Version of the library format (for future migrations)
    #[serde(default = "default_version")]
    pub version: String,
    /// All saved scripts
    #[serde(default)]
    pub scripts: Vec<PlaywrightScript>,
}

fn default_version() -> String {
    "1.0.0".to_string()
}

// ============================================================================
// File Operations
// ============================================================================

/// Get the playwright directory path in the app data directory
fn get_playwright_dir() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner")
        .join("playwright");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create playwright directory: {}", e))?;
    }

    Ok(app_data_dir)
}

/// Get the scripts file path
fn get_scripts_path() -> Result<PathBuf, String> {
    get_playwright_dir().map(|dir| dir.join(SCRIPTS_FILE))
}

/// Get the results directory for test reports
pub fn get_results_dir() -> Result<PathBuf, String> {
    let dir = get_playwright_dir()?.join("results");
    if !dir.exists() {
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create results directory: {}", e))?;
    }
    Ok(dir)
}

/// Load the script library from disk
pub fn load_script_library() -> PlaywrightLibrary {
    match get_scripts_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(library) => {
                            info!("Loaded playwright script library from {:?}", path);
                            library
                        }
                        Err(e) => {
                            error!("Failed to parse playwright scripts file: {}", e);
                            PlaywrightLibrary::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read playwright scripts file: {}", e);
                        PlaywrightLibrary::default()
                    }
                }
            } else {
                info!("No playwright scripts file found, using empty library");
                PlaywrightLibrary::default()
            }
        }
        Err(e) => {
            error!("Failed to get playwright scripts path: {}", e);
            PlaywrightLibrary::default()
        }
    }
}

/// Save the script library to disk
pub fn save_script_library(library: &PlaywrightLibrary) -> Result<(), String> {
    let path = get_scripts_path()?;

    let contents = serde_json::to_string_pretty(library)
        .map_err(|e| format!("Failed to serialize playwright scripts: {}", e))?;

    fs::write(&path, contents)
        .map_err(|e| format!("Failed to write playwright scripts file: {}", e))?;

    info!("Saved playwright script library to {:?}", path);
    Ok(())
}

// ============================================================================
// CRUD Operations
// ============================================================================

/// Get all scripts
pub fn get_all_scripts() -> Vec<PlaywrightScript> {
    load_script_library().scripts
}

/// Get a script by ID
pub fn get_script(id: &str) -> Option<PlaywrightScript> {
    load_script_library()
        .scripts
        .into_iter()
        .find(|s| s.id == id)
}

/// Create a new script
#[allow(clippy::too_many_arguments)]
pub fn create_script(
    name: String,
    description: String,
    ai_instructions: Option<String>,
    target_url: String,
    script_content: String,
    category: String,
    tags: Vec<String>,
    timeout_seconds: u32,
    display_mode: DisplayMode,
    browser: String,
) -> Result<PlaywrightScript, String> {
    let mut library = load_script_library();

    let script = PlaywrightScript::with_details(
        name,
        description,
        ai_instructions,
        target_url,
        script_content,
        category,
        tags,
        timeout_seconds,
        display_mode,
        browser,
    );
    let created = script.clone();

    library.scripts.push(script);
    save_script_library(&library)?;

    info!(
        "Created playwright script: {} ({})",
        created.name, created.id
    );
    Ok(created)
}

/// Update an existing script
#[allow(clippy::too_many_arguments)]
pub fn update_script(
    id: &str,
    name: Option<String>,
    description: Option<String>,
    ai_instructions: Option<String>,
    target_url: Option<String>,
    script_content: Option<String>,
    category: Option<String>,
    tags: Option<Vec<String>>,
    timeout_seconds: Option<u32>,
    display_mode: Option<DisplayMode>,
    browser: Option<String>,
) -> Result<PlaywrightScript, String> {
    let mut library = load_script_library();

    let script = library
        .scripts
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| format!("Script not found: {}", id))?;

    if let Some(name) = name {
        script.name = name;
    }
    if let Some(description) = description {
        script.description = description;
    }
    if let Some(ai_instructions) = ai_instructions {
        script.ai_instructions = Some(ai_instructions);
    }
    if let Some(target_url) = target_url {
        script.target_url = target_url;
    }
    if let Some(script_content) = script_content {
        script.script_content = script_content;
    }
    if let Some(category) = category {
        script.category = category;
    }
    if let Some(tags) = tags {
        script.tags = tags;
    }
    if let Some(timeout_seconds) = timeout_seconds {
        script.timeout_seconds = timeout_seconds;
    }
    if let Some(display_mode) = display_mode {
        script.display_mode = display_mode;
    }
    if let Some(browser) = browser {
        script.browser = browser;
    }

    script.modified_at = chrono::Utc::now().to_rfc3339();

    // Mark as locally modified if previously synced
    if script.sync_status == SyncStatus::Synced {
        script.sync_status = SyncStatus::LocalModified;
    }

    let updated = script.clone();
    save_script_library(&library)?;

    info!(
        "Updated playwright script: {} ({})",
        updated.name, updated.id
    );
    Ok(updated)
}

/// Delete a script by ID
pub fn delete_script(id: &str) -> Result<(), String> {
    let mut library = load_script_library();

    let initial_len = library.scripts.len();
    library.scripts.retain(|s| s.id != id);

    if library.scripts.len() == initial_len {
        return Err(format!("Script not found: {}", id));
    }

    save_script_library(&library)?;

    info!("Deleted playwright script: {}", id);
    Ok(())
}

/// Import scripts from a JSON array
pub fn import_scripts(scripts_json: &str) -> Result<Vec<PlaywrightScript>, String> {
    let imported: Vec<PlaywrightScript> = serde_json::from_str(scripts_json)
        .map_err(|e| format!("Failed to parse import JSON: {}", e))?;

    let mut library = load_script_library();
    let mut created = Vec::new();

    for mut script in imported {
        // Generate new ID to avoid conflicts
        script.id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        script.created_at = now.clone();
        script.modified_at = now;
        script.sync_status = SyncStatus::LocalOnly;
        script.cloud_id = None;
        script.cloud_version = None;
        script.last_synced_at = None;

        created.push(script.clone());
        library.scripts.push(script);
    }

    save_script_library(&library)?;

    info!("Imported {} playwright scripts", created.len());
    Ok(created)
}

/// Export all scripts as JSON
pub fn export_scripts() -> Result<String, String> {
    let library = load_script_library();
    serde_json::to_string_pretty(&library.scripts)
        .map_err(|e| format!("Failed to serialize scripts for export: {}", e))
}

/// Get all unique categories from existing scripts
pub fn get_categories() -> Vec<String> {
    let library = load_script_library();
    let mut categories: Vec<String> = library
        .scripts
        .iter()
        .map(|s| s.category.clone())
        .filter(|c| !c.is_empty())
        .collect();
    categories.sort();
    categories.dedup();
    categories
}

/// Get all unique tags from existing scripts
pub fn get_all_tags() -> Vec<String> {
    let library = load_script_library();
    let mut tags: Vec<String> = library
        .scripts
        .iter()
        .flat_map(|s| s.tags.clone())
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

/// Search scripts by name, description, category, or tags
pub fn search_scripts(query: &str) -> Vec<PlaywrightScript> {
    let query_lower = query.to_lowercase();
    let library = load_script_library();

    library
        .scripts
        .into_iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&query_lower)
                || s.description.to_lowercase().contains(&query_lower)
                || s.category.to_lowercase().contains(&query_lower)
                || s.target_url.to_lowercase().contains(&query_lower)
                || s.tags
                    .iter()
                    .any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect()
}

/// Duplicate a script with a new name
pub fn duplicate_script(id: &str, new_name: Option<String>) -> Result<PlaywrightScript, String> {
    let original = get_script(id).ok_or_else(|| format!("Script not found: {}", id))?;

    let name = new_name.unwrap_or_else(|| format!("{} (Copy)", original.name));

    create_script(
        name,
        original.description,
        original.ai_instructions,
        original.target_url,
        original.script_content,
        original.category,
        original.tags,
        original.timeout_seconds,
        original.display_mode,
        original.browser,
    )
}

// ============================================================================
// Execution
// ============================================================================

/// Generate a wrapper script that connects to an existing Chrome browser via CDP
/// instead of launching a new browser instance.
fn generate_cdp_wrapper_script(original_script: &str, _target_url: &str) -> String {
    // We need to transform the script to:
    // 1. Use chromium.connectOverCDP instead of launching a new browser
    // 2. Use the existing page from the connected browser
    // 3. Execute the test logic on that page
    // Note: We intentionally don't navigate - the test runs on whatever page is open

    format!(
        r#"import {{ chromium, test as baseTest, expect }} from '@playwright/test';

// CDP connection wrapper - connects to existing Chrome browser
// Chrome must be started with: chrome.exe --remote-debugging-port=9222

const test = baseTest.extend<{{ cdpPage: import('@playwright/test').Page }}>( {{
  cdpPage: async ({{}}, use) => {{
    // Connect to Chrome running with remote debugging
    const browser = await chromium.connectOverCDP('http://127.0.0.1:9222');

    // Get the first context (usually the main browser window)
    const contexts = browser.contexts();
    if (contexts.length === 0) {{
      throw new Error('No browser contexts found. Make sure Chrome has at least one window open.');
    }}
    const context = contexts[0];

    // Get the first page or create one
    const pages = context.pages();
    const page = pages.length > 0 ? pages[0] : await context.newPage();

    await use(page);

    // Don't close the browser - we want to keep the user's session
  }},
}});

// Re-export expect for use in tests
export {{ expect }};

// Original test adapted to use CDP connection
test('CDP: Test on existing browser', async ({{ cdpPage: page }}) => {{
  // Using the page as-is - no navigation, test runs on whatever page is open
  // This is the whole point of "Open Browser" mode!

  // ========================================
  // Original test logic extracted below:
  // ========================================

{extracted_test_body}
}});
"#,
        extracted_test_body = extract_test_body(original_script)
    )
}

/// Extract the test body from a Playwright script, removing the import and test wrapper
fn extract_test_body(script: &str) -> String {
    // Find the test body between the async ({ page }) => { and the closing });
    // This is a simplified extraction - it looks for common patterns

    let script = script.trim();

    // Try to find the test function body
    if let Some(start) = script.find("async ({ page })") {
        if let Some(arrow_pos) = script[start..].find("=>") {
            let body_start = start + arrow_pos + 2;
            // Find the opening brace
            if let Some(brace_start) = script[body_start..].find('{') {
                let actual_start = body_start + brace_start + 1;
                // Find the matching closing brace (simplified - just find the last });)
                if let Some(end) = script.rfind("});") {
                    let body = &script[actual_start..end];
                    // Indent the body content
                    return body
                        .lines()
                        .map(|line| format!("  {}", line))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
    }

    // Also try with destructured page in different formats
    if let Some(start) = script.find("async ({") {
        if let Some(arrow_pos) = script[start..].find("=>") {
            let body_start = start + arrow_pos + 2;
            if let Some(brace_start) = script[body_start..].find('{') {
                let actual_start = body_start + brace_start + 1;
                if let Some(end) = script.rfind("});") {
                    let body = &script[actual_start..end];
                    return body
                        .lines()
                        .map(|line| format!("  {}", line))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
    }

    // Fallback: return original script as a comment with a placeholder
    format!(
        "  // Could not extract test body automatically.\n  // Original script:\n{}",
        script
            .lines()
            .map(|line| format!("  // {}", line))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Execute a Playwright script and return the result
pub fn run_script(
    id: &str,
    target_url_override: Option<String>,
) -> Result<PlaywrightResult, String> {
    let script = get_script(id).ok_or_else(|| format!("Script not found: {}", id))?;

    // Find the qontinui-runner directory (where Playwright is installed in node_modules)
    // Test files MUST be placed in this directory for Node.js to find @playwright/test
    let exe_path =
        std::env::current_exe().map_err(|e| format!("Failed to get executable path: {}", e))?;

    info!("Executable path: {}", exe_path.display());

    // Walk up to find node_modules/@playwright directory
    let mut runner_dir = exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "Failed to get executable directory".to_string())?;

    for _ in 0..10 {
        if runner_dir.join("node_modules").join("@playwright").exists() {
            info!("Found node_modules at: {}", runner_dir.display());
            break;
        }
        if let Some(parent) = runner_dir.parent() {
            runner_dir = parent.to_path_buf();
        } else {
            break;
        }
    }

    // Verify we found @playwright/test
    if !runner_dir.join("node_modules").join("@playwright").exists() {
        return Err(format!(
            "Could not find @playwright/test in node_modules. \
             Make sure @playwright/test is installed in qontinui-runner. \
             Searched from: {}",
            exe_path.display()
        ));
    }

    // Create a user-scripts subdirectory in the runner directory for test files
    // This ensures Node.js can find @playwright/test from the test file's location
    let user_scripts_dir = runner_dir.join("playwright-user-scripts");
    fs::create_dir_all(&user_scripts_dir)
        .map_err(|e| format!("Failed to create user scripts directory: {}", e))?;

    // Write the script file
    let script_file = user_scripts_dir.join(format!("{}.spec.ts", id));

    // Prepare script content with potential URL override
    let mut content = script.script_content.clone();
    if let Some(url) = target_url_override {
        if content.contains("baseURL:") {
            let re = regex::Regex::new(r#"baseURL:\s*['"`][^'"`]*['"`]"#)
                .map_err(|e| format!("Regex error: {}", e))?;
            content = re
                .replace(&content, format!(r#"baseURL: '{}'"#, url))
                .to_string();
        }
    }

    // For CDP connection mode, wrap the script to connect to existing browser
    if script.display_mode == DisplayMode::ConnectExisting {
        content = generate_cdp_wrapper_script(&content, &script.target_url);
    }

    fs::write(&script_file, &content).map_err(|e| format!("Failed to write script file: {}", e))?;

    // Prepare output directory for this run
    let results_dir = get_results_dir()?;
    let run_id = Uuid::new_v4().to_string();
    let output_dir = results_dir.join(&run_id);
    fs::create_dir_all(&output_dir)
        .map_err(|e| format!("Failed to create output directory: {}", e))?;

    info!(
        "Executing playwright script: {} (browser: {}, display_mode: {:?})",
        script.name, script.browser, script.display_mode
    );

    let start_time = std::time::Instant::now();
    let system = std::env::consts::OS;

    // Build args - just use the filename since playwright.config.ts sets testDir
    let script_filename = format!("{}.spec.ts", id);
    let mut args = vec![
        "playwright".to_string(),
        "test".to_string(),
        script_filename,
        "--reporter=json".to_string(),
        format!("--output={}", output_dir.to_str().unwrap()),
        format!("--timeout={}", script.timeout_seconds * 1000),
    ];

    // Add headed flag based on display mode
    match script.display_mode {
        DisplayMode::Headless => {
            // Default behavior, no extra args needed
        }
        DisplayMode::Headed => {
            args.push("--headed".to_string());
        }
        DisplayMode::ConnectExisting => {
            // For CDP connection, we need to use headed mode and connect to existing browser
            // The script content should use connectOverCDP - see generate_cdp_wrapper_script
            args.push("--headed".to_string());
        }
    }

    let output = if system == "windows" {
        // On Windows, use cmd.exe to ensure npx is found via PATH
        let npx_args = std::iter::once("npx".to_string())
            .chain(args.into_iter())
            .collect::<Vec<_>>()
            .join(" ");

        info!(
            "Running Playwright via cmd.exe in dir {}: {}",
            runner_dir.display(),
            npx_args
        );

        Command::new("cmd.exe")
            .args(["/c", &npx_args])
            .current_dir(&runner_dir)
            .output()
            .map_err(|e| format!("Failed to execute playwright via cmd.exe: {}", e))?
    } else {
        info!(
            "Running Playwright in dir {}: npx {}",
            runner_dir.display(),
            args.join(" ")
        );
        Command::new("npx")
            .args(&args)
            .current_dir(&runner_dir)
            .output()
            .map_err(|e| format!("Failed to execute playwright: {}", e))?
    };

    // Clean up the script file after execution
    let _ = fs::remove_file(&script_file);

    let duration_ms = start_time.elapsed().as_millis() as u64;
    let executed_at = chrono::Utc::now().to_rfc3339();

    // Parse JSON output
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    info!("Playwright stdout length: {}", stdout.len());
    if !stderr.is_empty() {
        warn!("Playwright stderr: {}", stderr);
    }

    // Try to parse JSON reporter output
    let (tests_passed, tests_failed, tests_skipped, specs, error) = parse_playwright_json(&stdout);

    let passed = tests_failed == 0 && output.status.success();

    // Collect screenshots from output directory
    let screenshots = collect_screenshots(&output_dir);

    // Build structured output for AI analysis
    // Include both stdout (after JSON) and stderr for complete picture
    let mut console_lines: Vec<String> = Vec::new();

    // Add stderr (contains line reporter output showing step-by-step progress)
    for line in stderr.lines() {
        console_lines.push(line.to_string());
    }

    // Add any non-JSON stdout lines (might have useful info)
    for line in stdout.lines() {
        // Skip JSON lines (they start with { or are part of JSON structure)
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with('{')
            && !trimmed.starts_with('}')
            && !trimmed.starts_with('[')
            && !trimmed.starts_with(']')
            && !trimmed.starts_with('"')
        {
            console_lines.push(line.to_string());
        }
    }

    // Collect page snapshot from error-context.md files
    let page_snapshot = collect_error_context(&output_dir);

    let structured_output = StructuredTestOutput {
        specs,
        console_output: console_lines,
        network_requests: None,
        page_snapshot,
    };

    // Populate workflow_status if this is workflow automation
    let workflow_status = if script.is_workflow_automation || script.workflow_objective.is_some() {
        Some(WorkflowStatus {
            objective: script.workflow_objective.clone(),
            script_passed: passed,
            script_passed_note: Some(
                "Script execution succeeded. This is EXPECTED for workflow automation. \
                 Script success does NOT mean workflow success."
                    .to_string(),
            ),
            objective_verified: None, // Pending verification
            verification_method: Some("pending".to_string()),
            verification_notes: None,
            success_criteria: script.success_criteria.clone(),
            criteria_results: vec![],
            verification_hints: vec![
                "Check the final screenshot to verify the objective was achieved".to_string(),
                "Look at the page snapshot YAML for expected elements".to_string(),
                "Verify there are no error messages or toasts".to_string(),
            ],
        })
    } else {
        None
    };

    let result = PlaywrightResult {
        passed,
        tests_passed,
        tests_failed,
        tests_skipped,
        duration_ms,
        error,
        report_path: Some(output_dir.to_str().unwrap().to_string()),
        screenshots,
        video_paths: find_video_files(&output_dir),
        trace_path: find_trace_file(&output_dir),
        executed_at,
        structured_output: Some(structured_output),
        workflow_status,
    };

    // Save result to script's last_result
    let mut library = load_script_library();
    if let Some(s) = library.scripts.iter_mut().find(|s| s.id == id) {
        s.last_result = Some(result.clone());
        let _ = save_script_library(&library);
    }

    // Clean up temp file
    let _ = fs::remove_file(&script_file);

    info!(
        "Playwright execution complete: passed={}, tests_passed={}, tests_failed={}",
        result.passed, result.tests_passed, result.tests_failed
    );

    // Log to .dev-logs for AI Developer workflows
    let display_mode_str = match script.display_mode {
        DisplayMode::Headless => "headless",
        DisplayMode::Headed => "headed",
        DisplayMode::ConnectExisting => "connect_existing",
    };

    // Convert specs to tuple format for logging
    let spec_tuples: Vec<(String, String, u64, Option<String>)> = result
        .structured_output
        .as_ref()
        .map(|so| {
            so.specs
                .iter()
                .map(|s| {
                    (
                        s.title.clone(),
                        s.status.clone(),
                        s.duration_ms,
                        s.error.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    let console_output: Vec<String> = result
        .structured_output
        .as_ref()
        .map(|so| so.console_output.clone())
        .unwrap_or_default();

    let page_snapshot = result
        .structured_output
        .as_ref()
        .and_then(|so| so.page_snapshot.as_deref());

    FileLogger::log_playwright_execution(
        &script.id,
        &script.name,
        Some(&script.target_url),
        Some(display_mode_str),
        Some(&script.browser),
        result.passed,
        result.tests_passed,
        result.tests_failed,
        result.tests_skipped,
        result.duration_ms,
        result.error.as_deref(),
        &result.screenshots,
        &console_output,
        &spec_tuples,
        page_snapshot,
        result.workflow_status.clone(),
    );

    Ok(result)
}

/// Strip ANSI escape codes from a string
fn strip_ansi_codes(s: &str) -> String {
    // ANSI escape codes follow the pattern: ESC [ ... m
    // ESC is \x1b (27) or \033
    let re = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    re.replace_all(s, "").to_string()
}

/// Parse Playwright JSON reporter output
fn parse_playwright_json(output: &str) -> (u32, u32, u32, Vec<TestSpec>, Option<String>) {
    // Try to find JSON in the output (it might have other text before/after)
    let json_start = output.find('{');
    let json_end = output.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        let json_str = &output[start..=end];

        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
            let mut passed = 0u32;
            let mut failed = 0u32;
            let mut skipped = 0u32;
            let mut specs = Vec::new();
            let mut error_msg = None;

            // Parse suites/specs from Playwright JSON format
            if let Some(suites) = json.get("suites").and_then(|s| s.as_array()) {
                for suite in suites {
                    if let Some(suite_specs) = suite.get("specs").and_then(|s| s.as_array()) {
                        for spec in suite_specs {
                            let title = spec
                                .get("title")
                                .and_then(|t| t.as_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let file = spec
                                .get("file")
                                .and_then(|f| f.as_str())
                                .unwrap_or("")
                                .to_string();

                            // Get test results
                            if let Some(tests) = spec.get("tests").and_then(|t| t.as_array()) {
                                for test in tests {
                                    let status = test
                                        .get("status")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();

                                    let duration = test
                                        .get("results")
                                        .and_then(|r| r.as_array())
                                        .and_then(|r| r.first())
                                        .and_then(|r| r.get("duration"))
                                        .and_then(|d| d.as_u64())
                                        .unwrap_or(0);

                                    let test_error = test
                                        .get("results")
                                        .and_then(|r| r.as_array())
                                        .and_then(|r| r.first())
                                        .and_then(|r| r.get("error"))
                                        .and_then(|e| e.get("message"))
                                        .and_then(|m| m.as_str())
                                        .map(|s| strip_ansi_codes(s));

                                    match status.as_str() {
                                        "passed" | "expected" => passed += 1,
                                        "failed" | "unexpected" => {
                                            failed += 1;
                                            if error_msg.is_none() {
                                                error_msg = test_error.clone();
                                            }
                                        }
                                        "skipped" => skipped += 1,
                                        _ => {}
                                    }

                                    specs.push(TestSpec {
                                        title: title.clone(),
                                        file: file.clone(),
                                        status,
                                        duration_ms: duration,
                                        error: test_error,
                                        retry: 0,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            return (passed, failed, skipped, specs, error_msg);
        }
    }

    // If we couldn't parse JSON, check exit status
    if output.contains("passed") && !output.contains("failed") {
        (1, 0, 0, Vec::new(), None)
    } else if output.contains("failed") {
        (
            0,
            1,
            0,
            Vec::new(),
            Some("Test execution failed - see console output".to_string()),
        )
    } else {
        (
            0,
            0,
            0,
            Vec::new(),
            Some("Could not parse test results".to_string()),
        )
    }
}

/// Collect screenshot paths from output directory (recursive)
fn collect_screenshots(output_dir: &PathBuf) -> Vec<String> {
    let mut screenshots = Vec::new();

    fn collect_in_dir(dir: &PathBuf, screenshots: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    collect_in_dir(&path, screenshots);
                } else if let Some(ext) = path.extension() {
                    if ext == "png" || ext == "jpg" || ext == "jpeg" || ext == "webp" {
                        if let Some(path_str) = path.to_str() {
                            screenshots.push(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    collect_in_dir(output_dir, &mut screenshots);
    screenshots
}

/// Collect error context (page snapshot) from error-context.md files
fn collect_error_context(output_dir: &PathBuf) -> Option<String> {
    fn find_in_dir(dir: &PathBuf) -> Option<String> {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(found) = find_in_dir(&path) {
                        return Some(found);
                    }
                } else if path.file_name().map_or(false, |n| n == "error-context.md") {
                    if let Ok(content) = fs::read_to_string(&path) {
                        return Some(content);
                    }
                }
            }
        }
        None
    }

    find_in_dir(output_dir)
}

/// Find trace file in output directory (recursively checks subdirs)
fn find_trace_file(output_dir: &PathBuf) -> Option<String> {
    fn find_in_dir(dir: &PathBuf) -> Option<String> {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    if let Some(found) = find_in_dir(&path) {
                        return Some(found);
                    }
                } else if let Some(ext) = path.extension() {
                    if ext == "zip" && path.to_str().map_or(false, |s| s.contains("trace")) {
                        return path.to_str().map(String::from);
                    }
                }
            }
        }
        None
    }

    find_in_dir(output_dir)
}

/// Find video files in output directory (recursively checks subdirs)
fn find_video_files(output_dir: &PathBuf) -> Vec<String> {
    let mut videos = Vec::new();

    fn find_in_dir(dir: &PathBuf, videos: &mut Vec<String>) {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Recursively check subdirectories
                    find_in_dir(&path, videos);
                } else if let Some(ext) = path.extension() {
                    // Playwright records videos as .webm files
                    if ext == "webm" || ext == "mp4" {
                        if let Some(path_str) = path.to_str() {
                            videos.push(path_str.to_string());
                        }
                    }
                }
            }
        }
    }

    find_in_dir(output_dir, &mut videos);
    videos
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_script_creation() {
        let script = PlaywrightScript::new(
            "Test Script".to_string(),
            "test('example', async () => {});".to_string(),
        );
        assert!(!script.id.is_empty());
        assert_eq!(script.name, "Test Script");
        assert_eq!(script.timeout_seconds, 60);
        assert_eq!(script.display_mode, DisplayMode::Headless);
        assert_eq!(script.browser, "chromium");
        assert_eq!(script.sync_status, SyncStatus::LocalOnly);
    }

    #[test]
    fn test_script_with_details() {
        let script = PlaywrightScript::with_details(
            "E2E Test".to_string(),
            "Tests the login flow".to_string(),
            Some("Test the login flow end-to-end".to_string()),
            "http://localhost:3000".to_string(),
            "test content".to_string(),
            "E2E".to_string(),
            vec!["login".to_string(), "auth".to_string()],
            120,
            DisplayMode::Headed,
            "firefox".to_string(),
        );
        assert_eq!(script.category, "E2E");
        assert_eq!(script.timeout_seconds, 120);
        assert_eq!(script.display_mode, DisplayMode::Headed);
        assert_eq!(script.browser, "firefox");
    }

    #[test]
    fn test_sync_status_default() {
        let status = SyncStatus::default();
        assert_eq!(status, SyncStatus::LocalOnly);
    }
}
