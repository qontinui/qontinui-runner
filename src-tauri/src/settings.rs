use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

use crate::ai_router::RoutingConfig;
use crate::orchestrator::{CompressionConfig, RetryConfig};

const SETTINGS_FILE: &str = "settings.json";

// ============================================================================
// AI Settings
// ============================================================================

/// AI provider selection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AiProvider {
    #[default]
    ClaudeCli, // Claude Code CLI (subscription-based, recommended)
    ClaudeApi, // Claude API (per-token billing)
    GeminiCli, // Gemini CLI (OAuth or API key auth)
    GeminiApi, // Gemini API (direct HTTP calls)
}

/// CLI execution mode for Claude Code
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum CliExecutionMode {
    #[default]
    Auto, // Auto-detect based on platform
    WindowsNative, // Call claude.exe directly on Windows
    Wsl,           // Call via WSL
    Native,        // Native *nix execution
}

/// Settings for Claude Code CLI execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeCliSettings {
    pub execution_mode: CliExecutionMode,
    pub custom_path: Option<String>, // Custom path to claude executable
    pub timeout_seconds: u64,
    /// Custom CLAUDE_CONFIG_DIR for multi-account support
    /// e.g., "C:\\Users\\Name\\.claude-work" or "/home/user/.claude-personal"
    #[serde(default)]
    pub config_dir: Option<String>,
}

impl Default for ClaudeCliSettings {
    fn default() -> Self {
        Self {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            timeout_seconds: 600,
            config_dir: None,
        }
    }
}

/// Settings for Claude API (direct HTTP calls)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeApiSettings {
    pub model: String,
    pub max_tokens: u32,
    // Note: API key stored separately in OS keychain
}

impl Default for ClaudeApiSettings {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
        }
    }
}

/// Authentication method for Gemini CLI
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GeminiAuthMethod {
    #[default]
    OAuth, // Google Account OAuth (60 req/min, 1000 req/day free)
    ApiKey, // API Key via GEMINI_API_KEY env var (100 req/day free)
}

/// Settings for Gemini CLI execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiCliSettings {
    pub execution_mode: CliExecutionMode,
    pub custom_path: Option<String>, // Custom path to gemini executable
    pub timeout_seconds: u64,
    pub auth_method: GeminiAuthMethod,
    pub model: String, // Model to use (e.g., "gemini-3-flash-preview")
}

impl Default for GeminiCliSettings {
    fn default() -> Self {
        Self {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            timeout_seconds: 600,
            auth_method: GeminiAuthMethod::OAuth,
            model: "gemini-3-flash-preview".to_string(),
        }
    }
}

/// Settings for Gemini API (direct HTTP calls)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiApiSettings {
    pub model: String,
    pub max_output_tokens: u32,
    pub temperature: f32,
    // Note: API key stored separately in OS keychain
}

impl Default for GeminiApiSettings {
    fn default() -> Self {
        Self {
            model: "gemini-3-flash-preview".to_string(),
            max_output_tokens: 8192,
            temperature: 0.7,
        }
    }
}

/// Complete AI settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiSettings {
    pub provider: AiProvider,
    pub claude_cli: ClaudeCliSettings,
    pub claude_api: ClaudeApiSettings,
    #[serde(default)]
    pub gemini_cli: GeminiCliSettings,
    #[serde(default)]
    pub gemini_api: GeminiApiSettings,
    /// Default iteration threshold for including video in auto-refine (0 = never)
    #[serde(default = "default_auto_refine_video_after_iterations")]
    pub auto_refine_video_after_iterations: u32,
    /// Memory compression configuration for context management
    #[serde(default)]
    pub compression: CompressionConfig,
    /// Retry configuration for handling transient failures
    #[serde(default)]
    pub retry: RetryConfig,
    /// Task routing configuration for model selection based on complexity
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Enable interactive bidirectional CLI sessions (stream-json protocol).
    /// When true and a SessionManager is available, sessions use multi-turn interactive mode.
    /// When false, sessions always use the one-shot inline mode.
    #[serde(default = "default_interactive_sessions_enabled")]
    pub interactive_sessions_enabled: bool,
}

fn default_interactive_sessions_enabled() -> bool {
    true
}

fn default_auto_refine_video_after_iterations() -> u32 {
    3 // Include video after 3 failed iterations by default
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            provider: AiProvider::default(),
            claude_cli: ClaudeCliSettings::default(),
            claude_api: ClaudeApiSettings::default(),
            gemini_cli: GeminiCliSettings::default(),
            gemini_api: GeminiApiSettings::default(),
            auto_refine_video_after_iterations: default_auto_refine_video_after_iterations(),
            compression: CompressionConfig::default(),
            retry: RetryConfig::default(),
            routing: RoutingConfig::default(),
            interactive_sessions_enabled: default_interactive_sessions_enabled(),
        }
    }
}

// ============================================================================
// Playwright Settings
// ============================================================================

/// Settings for Playwright test execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaywrightSettings {
    /// Username or email for test authentication
    /// Passed as PLAYWRIGHT_TEST_USERNAME environment variable
    pub test_username: Option<String>,
    /// Password for test authentication
    /// Passed as PLAYWRIGHT_TEST_PASSWORD environment variable
    pub test_password: Option<String>,
    /// Base URL for tests (e.g., http://localhost:3001)
    /// Passed as PLAYWRIGHT_BASE_URL environment variable
    pub base_url: Option<String>,
    /// Skip web server startup (assume it's already running)
    /// Passed as SKIP_WEB_SERVER=1 environment variable
    #[serde(default = "default_skip_web_server")]
    pub skip_web_server: bool,
}

fn default_skip_web_server() -> bool {
    true // Default to true since runner users typically have servers running
}

impl Default for PlaywrightSettings {
    fn default() -> Self {
        Self {
            test_username: None,
            test_password: None,
            base_url: None,
            skip_web_server: default_skip_web_server(),
        }
    }
}

// ============================================================================
// Self-Healing Settings
// ============================================================================

/// LLM mode for self-healing operations
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelfHealingLlmMode {
    #[default]
    Disabled, // No LLM assistance
    LocalOllama, // Use local Ollama instance
    RemoteApi,   // Use remote API (OpenAI/Anthropic)
}

/// API provider for remote LLM
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelfHealingApiProvider {
    #[default]
    OpenAi,
    Anthropic,
}

/// Settings for self-healing automation features
/// These settings are passed to the qontinui Python library when executing workflows
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfHealingSettings {
    /// Enable action caching to avoid redundant operations
    #[serde(default = "default_action_caching_enabled")]
    pub action_caching_enabled: bool,
    /// Cache TTL in seconds (how long cached actions remain valid)
    #[serde(default = "default_cache_ttl_seconds")]
    pub cache_ttl_seconds: u32,
    /// Enable visual validation of actions
    #[serde(default = "default_visual_validation_enabled")]
    pub visual_validation_enabled: bool,
    /// LLM mode for self-healing assistance
    #[serde(default)]
    pub llm_mode: SelfHealingLlmMode,
    /// Ollama model name (used when llm_mode is LocalOllama)
    #[serde(default = "default_ollama_model")]
    pub ollama_model: String,
    /// API provider (used when llm_mode is RemoteApi)
    #[serde(default)]
    pub api_provider: SelfHealingApiProvider,
    // Note: API key stored separately in OS keychain
}

fn default_action_caching_enabled() -> bool {
    true
}

fn default_cache_ttl_seconds() -> u32 {
    300 // 5 minutes
}

fn default_visual_validation_enabled() -> bool {
    true
}

fn default_ollama_model() -> String {
    "llava".to_string()
}

impl Default for SelfHealingSettings {
    fn default() -> Self {
        Self {
            action_caching_enabled: default_action_caching_enabled(),
            cache_ttl_seconds: default_cache_ttl_seconds(),
            visual_validation_enabled: default_visual_validation_enabled(),
            llm_mode: SelfHealingLlmMode::default(),
            ollama_model: default_ollama_model(),
            api_provider: SelfHealingApiProvider::default(),
        }
    }
}

// ============================================================================
// Accessibility Settings
// ============================================================================

/// Settings for accessibility capture and Chrome DevTools Protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessibilitySettings {
    /// Path to Chrome/Chromium executable for launching with remote debugging
    /// If None, will try to auto-detect common installation paths
    pub chrome_path: Option<String>,
    /// Default CDP port for remote debugging (default: 9222)
    #[serde(default = "default_cdp_port")]
    pub cdp_port: u16,
}

fn default_cdp_port() -> u16 {
    9222
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            chrome_path: None, // Auto-detect
            cdp_port: default_cdp_port(),
        }
    }
}

// ============================================================================
// Mobile Settings
// ============================================================================

/// Settings for mobile development feedback (ADB, Android devices)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileSettings {
    /// Custom path to ADB executable (None = auto-detect)
    /// Example: "C:\\Users\\Name\\AppData\\Local\\Android\\Sdk\\platform-tools\\adb.exe"
    #[serde(default)]
    pub adb_path: Option<String>,

    /// Default device ID to use when multiple devices are connected (None = use first)
    #[serde(default)]
    pub default_device_id: Option<String>,

    /// App package name for filtering logcat output
    /// Example: "com.myapp" or "com.myapp.debug"
    #[serde(default)]
    pub app_package: Option<String>,

    /// Default number of logcat lines to capture
    #[serde(default = "default_logcat_lines")]
    pub logcat_lines: u32,

    /// Filter to React Native / Metro logs only when capturing logcat
    #[serde(default)]
    pub filter_react_native: bool,

    /// Custom output directory for mobile captures (screenshots, logs)
    /// If None, uses the project's screenshot/log directory
    #[serde(default)]
    pub output_dir: Option<String>,
}

fn default_logcat_lines() -> u32 {
    500
}

impl Default for MobileSettings {
    fn default() -> Self {
        Self {
            adb_path: None,
            default_device_id: None,
            app_package: None,
            logcat_lines: default_logcat_lines(),
            filter_react_native: false,
            output_dir: None,
        }
    }
}

// ============================================================================
// Global Log Source Settings
// ============================================================================

/// AI selection mode for log sources
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogSourceAiSelectionMode {
    /// AI selects relevant sources at the start of each verification round
    #[default]
    Dynamic,
    /// AI selects relevant sources once at workflow setup
    Static,
    /// No AI selection - use explicit profile or all enabled sources
    Disabled,
}

/// Category for log sources to help AI understand their purpose
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LogSourceCategory {
    /// Web frontend logs (Next.js, React, Vite, etc.)
    Frontend,
    /// Web backend logs (FastAPI, Express, Django, etc.)
    Backend,
    /// API/service logs
    Api,
    /// Mobile app logs (logcat, Metro bundler, etc.)
    Mobile,
    /// Database logs
    Database,
    /// Build/CI logs
    Build,
    /// Test runner logs (Playwright, Jest, pytest, etc.)
    Testing,
    /// Qontinui runner internal logs
    Runner,
    /// General/uncategorized logs
    #[default]
    General,
}

/// A single global log source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalLogSource {
    /// Unique identifier
    pub id: String,
    /// Human-readable name (e.g., "Backend", "Metro Bundler")
    pub name: String,
    /// Description for AI to understand what this source contains
    /// e.g., "FastAPI backend logs including HTTP requests and errors"
    pub description: String,
    /// Category to help AI filter relevant sources
    #[serde(default)]
    pub category: LogSourceCategory,
    /// Type: "file" or "directory"
    #[serde(rename = "type")]
    pub source_type: String,
    /// Absolute path to log file or directory
    pub path: String,
    /// Glob pattern for directory type (e.g., "*.log")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Number of lines to tail (default: 100)
    #[serde(default = "default_tail_lines")]
    pub tail_lines: u32,
    /// Whether this source is globally enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional color for UI display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Keywords that help AI identify when this source is relevant
    /// e.g., ["python", "fastapi", "http", "api"]
    #[serde(default)]
    pub keywords: Vec<String>,

    // --- Error monitoring fields ---
    /// Log format: "plaintext", "json", or "jsonl"
    #[serde(default = "default_format")]
    pub format: String,
    /// Parser type: "python", "javascript", "rust", or "generic"
    #[serde(default = "default_parser")]
    pub parser: String,
    /// Regex pattern to extract timestamps from log lines
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_pattern: Option<String>,
    /// Timezone for parsing timestamps (default: "local")
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Custom regex patterns to identify errors
    #[serde(default)]
    pub error_patterns: Vec<String>,
    /// Custom regex patterns to identify warnings
    #[serde(default)]
    pub warning_patterns: Vec<String>,
    /// Patterns to ignore (suppress false positives)
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
    /// Polling interval in milliseconds for error monitoring
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u32,
}

fn default_tail_lines() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

fn default_format() -> String {
    "plaintext".to_string()
}

fn default_parser() -> String {
    "generic".to_string()
}

fn default_timezone() -> String {
    "local".to_string()
}

fn default_poll_interval_ms() -> u32 {
    5000
}

/// A named profile grouping log source IDs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalLogSourceProfile {
    /// Unique identifier
    pub id: String,
    /// Human-readable name (e.g., "Web Development", "Mobile Development")
    pub name: String,
    /// Description of what this profile is for
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// IDs of log sources included in this profile
    pub source_ids: Vec<String>,
    /// When this profile was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// When this profile was last modified
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Global log source settings - shared across all projects
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalLogSourceSettings {
    /// All available log sources
    #[serde(default)]
    pub sources: Vec<GlobalLogSource>,
    /// Named profiles for grouping sources
    #[serde(default)]
    pub profiles: Vec<GlobalLogSourceProfile>,
    /// Default profile to use when no explicit selection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile_id: Option<String>,
    /// How AI should select relevant log sources
    #[serde(default)]
    pub ai_selection_mode: LogSourceAiSelectionMode,
    /// Whether to include all enabled sources when AI selection is disabled and no profile is set
    #[serde(default = "default_true")]
    pub include_all_when_no_profile: bool,
}

impl Default for GlobalLogSourceSettings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            profiles: Vec::new(),
            default_profile_id: None,
            ai_selection_mode: LogSourceAiSelectionMode::Dynamic,
            include_all_when_no_profile: true,
        }
    }
}

// ============================================================================
// Path Settings
// ============================================================================

/// Configurable paths for file system operations.
///
/// All paths have sensible cross-platform defaults but can be overridden
/// for development or custom deployments.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathSettings {
    /// Base directory for development/debug logs (JSONL files, screenshots, etc.)
    ///
    /// Default (when None):
    /// - Windows: `C:\Users\<user>\AppData\Local\qontinui-runner\dev-logs`
    /// - macOS: `~/Library/Application Support/qontinui-runner/dev-logs`
    /// - Linux: `~/.local/share/qontinui-runner/dev-logs`
    ///
    /// Override example: `D:\qontinui_parent_directory\.dev-logs`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_logs_dir: Option<String>,
}

// ============================================================================
// Execution Variables Settings
// ============================================================================

/// Authentication source for test execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthSource {
    /// Use captured headers from saved requests (original tokens may expire)
    #[default]
    Captured,
    /// Use manually configured auth token
    Manual,
    /// Use environment variable for auth token
    Environment,
}

/// A custom variable for execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomVariable {
    /// Variable name (used as {{name}} in substitution)
    pub name: String,
    /// Source of the value: "manual" or "environment"
    #[serde(default)]
    pub source: VariableSource,
    /// Manual value (when source is "manual")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Environment variable name (when source is "environment")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    /// Optional description for the variable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Source of a variable value
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum VariableSource {
    #[default]
    Manual,
    Environment,
}

/// Settings for execution variables
///
/// Allows configuring authentication source and custom variables
/// for API request execution in the Test Orchestrator and similar features.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionVariablesSettings {
    /// Authentication source for API requests
    #[serde(default)]
    pub auth_source: AuthSource,

    /// Header name to use for authentication (default: "Authorization")
    #[serde(default = "default_auth_header_name")]
    pub auth_header_name: String,

    /// Manual auth token (when auth_source is "manual")
    /// Note: For security, sensitive tokens should ideally be stored in keychain
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,

    /// Environment variable name for auth token (when auth_source is "environment")
    #[serde(default = "default_auth_env_var")]
    pub auth_env_var: String,

    /// Custom variables for substitution
    #[serde(default)]
    pub custom_variables: Vec<CustomVariable>,
}

fn default_auth_header_name() -> String {
    "Authorization".to_string()
}

fn default_auth_env_var() -> String {
    "API_AUTH_TOKEN".to_string()
}

impl Default for ExecutionVariablesSettings {
    fn default() -> Self {
        Self {
            auth_source: AuthSource::default(),
            auth_header_name: default_auth_header_name(),
            auth_token: None,
            auth_env_var: default_auth_env_var(),
            custom_variables: Vec::new(),
        }
    }
}

// ============================================================================
// Debug Settings
// ============================================================================

/// Debug settings for image matching and other diagnostic features
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugSettings {
    /// Enable detailed image matching debug information
    pub enable_image_debug: bool,
    /// Number of top matches to include in debug output
    pub top_matches_count: u32,
}

impl Default for DebugSettings {
    fn default() -> Self {
        Self {
            // Default to true to enable visual debug image generation for troubleshooting
            enable_image_debug: true,
            top_matches_count: 5,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    last_config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_monitor_index: Option<i32>,
    /// Multi-monitor selection support (takes precedence over last_monitor_index)
    #[serde(skip_serializing_if = "Option::is_none")]
    last_monitor_indices: Option<Vec<i32>>,
    #[serde(default = "default_auto_load_last_config")]
    pub auto_load_last_config: bool,
    /// Auto-continue AI Developer workflows after runner restart (default: true)
    #[serde(default = "default_auto_continue_ai_workflow")]
    pub auto_continue_ai_workflow: bool,
    /// Auto-fix issues on session failure (triggers auto-fix when workflow/prompt fails)
    #[serde(default)]
    pub session_auto_fix_on_failure: bool,
    /// Include AI Summary step in new workflows by default (default: true)
    #[serde(default = "default_include_summary_step")]
    pub include_summary_step_by_default: bool,
    #[serde(default)]
    pub debug: DebugSettings,
    #[serde(default)]
    pub ai: AiSettings,
    #[serde(default)]
    pub playwright: PlaywrightSettings,
    #[serde(default)]
    pub accessibility: AccessibilitySettings,
    #[serde(default)]
    pub self_healing: SelfHealingSettings,
    #[serde(default)]
    pub paths: PathSettings,
    #[serde(default)]
    pub mobile: MobileSettings,
    #[serde(default)]
    pub log_sources: GlobalLogSourceSettings,
    #[serde(default)]
    pub execution_variables: ExecutionVariablesSettings,
    /// Global default: Run pre-flight environment check at start of Setup phase (default: true)
    /// Can be overridden per-workflow. Checks disk space, Node.js, Python, Rust, Git availability.
    #[serde(default = "default_preflight_check_enabled")]
    pub preflight_check_enabled: bool,
}

fn default_auto_load_last_config() -> bool {
    true
}

fn default_preflight_check_enabled() -> bool {
    true
}

fn default_auto_continue_ai_workflow() -> bool {
    true
}

fn default_include_summary_step() -> bool {
    true
}

/// Get the settings file path in the app data directory
fn get_settings_path() -> Result<PathBuf, String> {
    let app_data_dir = dirs::config_dir()
        .ok_or("Failed to get config directory")?
        .join("com.qontinui.runner");

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok(app_data_dir.join(SETTINGS_FILE))
}

/// Load settings from file
pub fn load_settings() -> Settings {
    match get_settings_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str(&contents) {
                        Ok(settings) => settings,
                        Err(e) => {
                            error!("Failed to parse settings file: {}", e);
                            Settings::default()
                        }
                    },
                    Err(e) => {
                        error!("Failed to read settings file: {}", e);
                        Settings::default()
                    }
                }
            } else {
                Settings::default()
            }
        }
        Err(e) => {
            error!("Failed to get settings path: {}", e);
            Settings::default()
        }
    }
}

/// Save settings to file
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = get_settings_path()?;
    let contents = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    fs::write(&path, contents).map_err(|e| format!("Failed to write settings file: {}", e))?;

    Ok(())
}

/// Save the last loaded config path
pub fn save_last_config_path(path: &str) -> Result<(), String> {
    info!("Saving last config path: {}", path);
    let mut settings = load_settings();
    settings.last_config_path = Some(path.to_string());
    save_settings(&settings)?;
    Ok(())
}

/// Get the last loaded config path
pub fn get_last_config_path() -> Option<String> {
    let settings = load_settings();
    settings.last_config_path
}

/// Get the current debug settings
pub fn get_debug_settings() -> DebugSettings {
    let settings = load_settings();
    settings.debug
}

/// Save debug settings
pub fn save_debug_settings(debug_settings: DebugSettings) -> Result<(), String> {
    info!("Saving debug settings: {:?}", debug_settings);
    let mut settings = load_settings();
    settings.debug = debug_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Save the last used workflow ID
pub fn save_last_workflow_id(workflow_id: &str) -> Result<(), String> {
    info!("Saving last workflow ID: {}", workflow_id);
    let mut settings = load_settings();
    settings.last_workflow_id = Some(workflow_id.to_string());
    save_settings(&settings)?;
    Ok(())
}

/// Get the last used workflow ID
pub fn get_last_workflow_id() -> Option<String> {
    let settings = load_settings();
    settings.last_workflow_id
}

/// Save the last used monitor index
pub fn save_last_monitor_index(monitor_index: i32) -> Result<(), String> {
    info!("Saving last monitor index: {}", monitor_index);
    let mut settings = load_settings();
    settings.last_monitor_index = Some(monitor_index);
    save_settings(&settings)?;
    Ok(())
}

/// Get the last used monitor index
pub fn get_last_monitor_index() -> Option<i32> {
    let settings = load_settings();
    settings.last_monitor_index
}

/// Save the last used monitor indices (multi-monitor support)
pub fn save_last_monitor_indices(monitor_indices: Vec<i32>) -> Result<(), String> {
    info!("Saving last monitor indices: {:?}", monitor_indices);
    let mut settings = load_settings();
    settings.last_monitor_indices = Some(monitor_indices.clone());
    // Also update legacy single monitor for backward compatibility
    if let Some(first) = monitor_indices.first() {
        settings.last_monitor_index = Some(*first);
    }
    save_settings(&settings)?;
    Ok(())
}

/// Get the last used monitor indices (multi-monitor support)
/// Falls back to legacy single monitor index if not set
pub fn get_last_monitor_indices() -> Option<Vec<i32>> {
    let settings = load_settings();
    // Prefer new multi-monitor setting, fall back to legacy single monitor
    settings
        .last_monitor_indices
        .or_else(|| settings.last_monitor_index.map(|idx| vec![idx]))
}

/// Get the auto-load last config setting
pub fn get_auto_load_last_config() -> bool {
    let settings = load_settings();
    settings.auto_load_last_config
}

/// Save the auto-load last config setting
pub fn save_auto_load_last_config(enabled: bool) -> Result<(), String> {
    info!("Saving auto-load last config setting: {}", enabled);
    let mut settings = load_settings();
    settings.auto_load_last_config = enabled;
    save_settings(&settings)?;
    Ok(())
}

/// Get the current AI settings
pub fn get_ai_settings() -> AiSettings {
    crate::config_facade::get_setting::<AiSettings>()
}

/// Save AI settings
pub fn save_ai_settings(ai_settings: AiSettings) -> Result<(), String> {
    crate::config_facade::save_setting(ai_settings)
}

/// Get the interactive sessions enabled setting
pub fn get_interactive_sessions_enabled() -> bool {
    let settings = load_settings();
    settings.ai.interactive_sessions_enabled
}

/// Save the interactive sessions enabled setting
pub fn save_interactive_sessions_enabled(enabled: bool) -> Result<(), String> {
    info!("Saving interactive sessions enabled setting: {}", enabled);
    let mut settings = load_settings();
    settings.ai.interactive_sessions_enabled = enabled;
    save_settings(&settings)
}

/// Get the auto-continue AI workflow setting
pub fn get_auto_continue_ai_workflow() -> bool {
    let settings = load_settings();
    settings.auto_continue_ai_workflow
}

/// Save the auto-continue AI workflow setting
pub fn save_auto_continue_ai_workflow(enabled: bool) -> Result<(), String> {
    info!("Saving auto-continue AI workflow setting: {}", enabled);
    let mut settings = load_settings();
    settings.auto_continue_ai_workflow = enabled;
    save_settings(&settings)?;
    Ok(())
}

/// Get the current Playwright settings
pub fn get_playwright_settings() -> PlaywrightSettings {
    crate::config_facade::get_setting::<PlaywrightSettings>()
}

/// Save Playwright settings
pub fn save_playwright_settings(playwright_settings: PlaywrightSettings) -> Result<(), String> {
    crate::config_facade::save_setting(playwright_settings)
}

/// Get the session auto-fix on failure setting
#[allow(dead_code)]
pub fn get_session_auto_fix_on_failure() -> bool {
    let settings = load_settings();
    settings.session_auto_fix_on_failure
}

/// Save the session auto-fix on failure setting
#[allow(dead_code)]
pub fn save_session_auto_fix_on_failure(enabled: bool) -> Result<(), String> {
    info!("Saving session auto-fix on failure setting: {}", enabled);
    let mut settings = load_settings();
    settings.session_auto_fix_on_failure = enabled;
    save_settings(&settings)?;
    Ok(())
}

/// Get the include summary step by default setting
pub fn get_include_summary_step_by_default() -> bool {
    let settings = load_settings();
    settings.include_summary_step_by_default
}

/// Save the include summary step by default setting
pub fn save_include_summary_step_by_default(enabled: bool) -> Result<(), String> {
    info!(
        "Saving include summary step by default setting: {}",
        enabled
    );
    let mut settings = load_settings();
    settings.include_summary_step_by_default = enabled;
    save_settings(&settings)?;
    Ok(())
}

/// Get the current Accessibility settings
pub fn get_accessibility_settings() -> AccessibilitySettings {
    let settings = load_settings();
    settings.accessibility
}

/// Save Accessibility settings
pub fn save_accessibility_settings(
    accessibility_settings: AccessibilitySettings,
) -> Result<(), String> {
    info!(
        "Saving Accessibility settings: {:?}",
        accessibility_settings
    );
    let mut settings = load_settings();
    settings.accessibility = accessibility_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Get the current Self-Healing settings
pub fn get_self_healing_settings() -> SelfHealingSettings {
    let settings = load_settings();
    settings.self_healing
}

/// Save Self-Healing settings
pub fn save_self_healing_settings(
    self_healing_settings: SelfHealingSettings,
) -> Result<(), String> {
    info!("Saving Self-Healing settings: {:?}", self_healing_settings);
    let mut settings = load_settings();
    settings.self_healing = self_healing_settings;
    save_settings(&settings)?;
    Ok(())
}

// ============================================================================
// Path Settings
// ============================================================================

/// Get the current Path settings
pub fn get_path_settings() -> PathSettings {
    let settings = load_settings();
    settings.paths
}

/// Get the dev_logs_dir override (used by paths module)
pub fn get_dev_logs_dir_override() -> Option<String> {
    let settings = load_settings();
    settings.paths.dev_logs_dir
}

/// Save Path settings
pub fn save_path_settings(path_settings: PathSettings) -> Result<(), String> {
    info!("Saving Path settings: {:?}", path_settings);
    let mut settings = load_settings();
    settings.paths = path_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Save the dev_logs_dir override
pub fn save_dev_logs_dir(dev_logs_dir: Option<String>) -> Result<(), String> {
    info!("Saving dev_logs_dir: {:?}", dev_logs_dir);
    let mut settings = load_settings();
    settings.paths.dev_logs_dir = dev_logs_dir;
    save_settings(&settings)?;
    Ok(())
}

// ============================================================================
// Mobile Settings
// ============================================================================

/// Get the current Mobile settings
pub fn get_mobile_settings() -> MobileSettings {
    let settings = load_settings();
    settings.mobile
}

/// Save Mobile settings
pub fn save_mobile_settings(mobile_settings: MobileSettings) -> Result<(), String> {
    info!("Saving Mobile settings: {:?}", mobile_settings);
    let mut settings = load_settings();
    settings.mobile = mobile_settings;
    save_settings(&settings)?;
    Ok(())
}

// ============================================================================
// Global Log Source Settings
// ============================================================================

/// Get the current Global Log Source settings
pub fn get_global_log_source_settings() -> GlobalLogSourceSettings {
    let settings = load_settings();
    settings.log_sources
}

/// Save Global Log Source settings
pub fn save_global_log_source_settings(
    log_source_settings: GlobalLogSourceSettings,
) -> Result<(), String> {
    info!(
        "Saving Global Log Source settings: {} sources, {} profiles",
        log_source_settings.sources.len(),
        log_source_settings.profiles.len()
    );
    let mut settings = load_settings();
    settings.log_sources = log_source_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Get enabled log sources, optionally filtered by profile
pub fn get_enabled_log_sources(profile_id: Option<&str>) -> Vec<GlobalLogSource> {
    let settings = get_global_log_source_settings();

    // If profile specified, filter by profile's source_ids
    if let Some(pid) = profile_id {
        if let Some(profile) = settings.profiles.iter().find(|p| p.id == pid) {
            return settings
                .sources
                .into_iter()
                .filter(|s| s.enabled && profile.source_ids.contains(&s.id))
                .collect();
        }
    }

    // If default profile is set and no explicit profile, use default
    if let Some(default_pid) = &settings.default_profile_id {
        if let Some(profile) = settings.profiles.iter().find(|p| p.id == *default_pid) {
            return settings
                .sources
                .into_iter()
                .filter(|s| s.enabled && profile.source_ids.contains(&s.id))
                .collect();
        }
    }

    // Fall back to all enabled sources if configured to do so
    if settings.include_all_when_no_profile {
        settings.sources.into_iter().filter(|s| s.enabled).collect()
    } else {
        Vec::new()
    }
}

/// Seed default log sources if the sources list is empty.
///
/// Called on startup to ensure there are always some log sources configured.
/// If the user has any sources (even disabled ones), this is a no-op.
/// Creates 4 default sources: Backend, Frontend, Runner, Runner Actions.
pub fn seed_default_log_sources_if_empty() {
    let settings = get_global_log_source_settings();
    if !settings.sources.is_empty() {
        return;
    }

    info!("No log sources configured, seeding defaults");
    let dev_logs_dir = crate::paths::get_dev_logs_dir();
    let dev_logs = dev_logs_dir.to_string_lossy().to_string();

    let defaults = vec![
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Backend".to_string(),
            description: "FastAPI backend logs including HTTP requests and errors".to_string(),
            category: LogSourceCategory::Backend,
            source_type: "file".to_string(),
            path: format!("{}/backend.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#22c55e".to_string()),
            keywords: vec![
                "python".to_string(),
                "fastapi".to_string(),
                "http".to_string(),
                "api".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "python".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Frontend".to_string(),
            description: "Next.js frontend logs including build and runtime errors".to_string(),
            category: LogSourceCategory::Frontend,
            source_type: "file".to_string(),
            path: format!("{}/frontend.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#3b82f6".to_string()),
            keywords: vec![
                "javascript".to_string(),
                "nextjs".to_string(),
                "react".to_string(),
                "frontend".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "javascript".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Runner".to_string(),
            description: "Qontinui runner Vite + Rust logs".to_string(),
            category: LogSourceCategory::Runner,
            source_type: "file".to_string(),
            path: format!("{}/runner-tauri.log", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#f97316".to_string()),
            keywords: vec![
                "rust".to_string(),
                "tauri".to_string(),
                "runner".to_string(),
                "vite".to_string(),
            ],
            format: "plaintext".to_string(),
            parser: "rust".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
        GlobalLogSource {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Runner Actions".to_string(),
            description: "Workflow action execution events in JSONL format".to_string(),
            category: LogSourceCategory::Runner,
            source_type: "file".to_string(),
            path: format!("{}/runner-actions.jsonl", dev_logs.replace('\\', "/")),
            pattern: None,
            tail_lines: 100,
            enabled: true,
            color: Some("#a855f7".to_string()),
            keywords: vec![
                "actions".to_string(),
                "workflow".to_string(),
                "execution".to_string(),
            ],
            format: "jsonl".to_string(),
            parser: "generic".to_string(),
            timestamp_pattern: None,
            timezone: "local".to_string(),
            error_patterns: vec![],
            warning_patterns: vec![],
            ignore_patterns: vec![],
            poll_interval_ms: 5000,
        },
    ];

    let new_settings = GlobalLogSourceSettings {
        sources: defaults,
        ..settings
    };

    if let Err(e) = save_global_log_source_settings(new_settings) {
        tracing::error!("Failed to seed default log sources: {}", e);
    } else {
        info!("Seeded 4 default log sources");
    }
}

/// Get log sources for AI selection prompt
/// Returns sources with their descriptions and categories for AI to choose from
pub fn get_log_sources_for_ai_selection() -> Vec<GlobalLogSource> {
    let settings = get_global_log_source_settings();
    settings.sources.into_iter().filter(|s| s.enabled).collect()
}

/// Get the AI selection mode
pub fn get_log_source_ai_selection_mode() -> LogSourceAiSelectionMode {
    let settings = get_global_log_source_settings();
    settings.ai_selection_mode
}

// ============================================================================
// Execution Variables Settings
// ============================================================================

/// Get the current Execution Variables settings
pub fn get_execution_variables_settings() -> ExecutionVariablesSettings {
    let settings = load_settings();
    settings.execution_variables
}

/// Save Execution Variables settings
pub fn save_execution_variables_settings(
    execution_variables_settings: ExecutionVariablesSettings,
) -> Result<(), String> {
    info!(
        "Saving Execution Variables settings: auth_source={:?}, {} custom variables",
        execution_variables_settings.auth_source,
        execution_variables_settings.custom_variables.len()
    );
    let mut settings = load_settings();
    settings.execution_variables = execution_variables_settings;
    save_settings(&settings)?;
    Ok(())
}

/// Get the resolved auth token based on current settings
///
/// Returns the auth token to use based on the configured auth source:
/// - Captured: Returns None (use original captured headers)
/// - Manual: Returns the manually configured token
/// - Environment: Returns the value from the configured environment variable
pub fn get_resolved_auth_token() -> Option<String> {
    let settings = get_execution_variables_settings();

    match settings.auth_source {
        AuthSource::Captured => None, // Use original captured headers
        AuthSource::Manual => settings.auth_token,
        AuthSource::Environment => std::env::var(&settings.auth_env_var).ok(),
    }
}

/// Get resolved custom variables
///
/// Returns all custom variables with their resolved values.
/// For environment variables, resolves them from the environment.
pub fn get_resolved_custom_variables() -> HashMap<String, String> {
    let settings = get_execution_variables_settings();
    let mut resolved = HashMap::new();

    for var in settings.custom_variables {
        let value = match var.source {
            VariableSource::Manual => var.value,
            VariableSource::Environment => var
                .env_var
                .and_then(|env_name| std::env::var(&env_name).ok()),
        };

        if let Some(v) = value {
            resolved.insert(var.name, v);
        }
    }

    resolved
}
