use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::error;

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

/// Account selection strategy for multi-account Claude CLI setups
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountSelectionMode {
    #[default]
    Manual, // Use the explicitly configured config_dir
    LeastUsage, // Auto-select the account with lowest utilization
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
    /// How to select which account to use when multiple config dirs exist
    #[serde(default)]
    pub account_selection_mode: AccountSelectionMode,
}

impl Default for ClaudeCliSettings {
    fn default() -> Self {
        Self {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            timeout_seconds: 600,
            config_dir: None,
            account_selection_mode: AccountSelectionMode::Manual,
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
    #[serde(rename = "type", default = "default_source_type")]
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

fn default_source_type() -> String {
    "file".to_string()
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

    /// When true, enforce workspace-scoped working directory resolution globally.
    /// Steps cannot resolve paths outside the workspace root.
    /// Default: false (permissive). Individual workflows can override via `strict_cwd`.
    #[serde(default)]
    pub strict_mode: bool,
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

// ============================================================================
// Cloud Relay Settings
// ============================================================================

/// Cloud relay settings for remote mobile access via backend WebSocket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRelaySettings {
    /// Whether cloud relay is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Backend URL to connect to
    #[serde(default = "default_backend_url")]
    pub backend_url: String,
    /// Auto-connect on app startup
    #[serde(default)]
    pub auto_connect: bool,
}

fn default_backend_url() -> String {
    "https://qontinui.io".to_string()
}

impl Default for CloudRelaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            backend_url: default_backend_url(),
            auto_connect: false,
        }
    }
}

// ============================================================================
// Memory Consolidation Settings
// ============================================================================

/// Settings for the memory consolidation service that synthesizes observations
/// into mental models with importance-weighted decay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConsolidationSettings {
    /// Minimum number of observations in a group to trigger consolidation (default: 3)
    #[serde(default = "default_min_group_size")]
    pub min_group_size: usize,
    /// Minimum hours between consolidation runs (default: 6.0)
    #[serde(default = "default_cooldown_hours")]
    pub cooldown_hours: f64,
    /// Retention threshold — observations below this are archived (default: 0.05)
    #[serde(default = "default_archive_threshold")]
    pub archive_threshold: f64,
    /// Maximum observations to scan per consolidation run (default: 500)
    #[serde(default = "default_max_observations")]
    pub max_observations: i64,
    /// Model override for consolidation LLM calls (use lightweight Haiku-class)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    /// Provider override for consolidation LLM calls
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_override: Option<String>,
}

fn default_min_group_size() -> usize { 3 }
fn default_cooldown_hours() -> f64 { 6.0 }
fn default_archive_threshold() -> f64 { 0.05 }
fn default_max_observations() -> i64 { 500 }

impl Default for MemoryConsolidationSettings {
    fn default() -> Self {
        Self {
            min_group_size: default_min_group_size(),
            cooldown_hours: default_cooldown_hours(),
            archive_threshold: default_archive_threshold(),
            max_observations: default_max_observations(),
            model_override: None,
            provider_override: None,
        }
    }
}

impl From<&MemoryConsolidationSettings> for crate::memory::consolidation::ConsolidationConfig {
    fn from(s: &MemoryConsolidationSettings) -> Self {
        Self {
            min_group_size: s.min_group_size,
            cooldown_hours: s.cooldown_hours,
            archive_threshold: s.archive_threshold,
            max_observations: s.max_observations,
            model_override: s.model_override.clone(),
            provider_override: s.provider_override.clone(),
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_config_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_workflow_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_monitor_index: Option<i32>,
    /// Multi-monitor selection support (takes precedence over last_monitor_index)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_monitor_indices: Option<Vec<i32>>,
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
    /// Managed process configurations for process capture
    #[serde(default)]
    pub managed_processes: Vec<crate::process_capture::ProcessConfig>,
    #[serde(default)]
    pub execution_variables: ExecutionVariablesSettings,
    /// Global default: Run pre-flight environment check at start of Setup phase (default: true)
    /// Can be overridden per-workflow. Checks disk space, Node.js, Python, Rust, Git availability.
    #[serde(default = "default_preflight_check_enabled")]
    pub preflight_check_enabled: bool,
    /// App mode: "simple" or "advanced" — synced across runner and web apps
    #[serde(default = "default_app_mode")]
    pub app_mode: String,
    /// Whether the first-launch setup wizard has been completed
    #[serde(default)]
    pub setup_completed: bool,
    /// Cloud relay settings for remote mobile access via backend WebSocket
    #[serde(default)]
    pub cloud_relay: CloudRelaySettings,
    /// Configured runner instances for multi-instance dev workflows
    #[serde(default)]
    pub runner_instances: Vec<RunnerInstanceConfig>,
    /// Directories containing Claude Code configs (each should have a `projects/` subdirectory).
    /// Used by Terminal > Browse Sessions to find transcript files.
    #[serde(default)]
    pub claude_config_dirs: Vec<String>,
    /// OpenTelemetry configuration for optional OTLP trace export.
    /// Note: OTel cannot be hot-reloaded; changes require a runner restart.
    #[serde(default)]
    pub otel: crate::otel::OtelConfig,
    #[serde(default)]
    pub container: crate::container::container_config::ContainerConfig,
    /// Memory consolidation settings (importance decay, grouping, LLM model)
    #[serde(default)]
    pub memory_consolidation: MemoryConsolidationSettings,
}

// ============================================================================
// Runner Instance Configuration (Dev Feature)
// ============================================================================

/// Configuration for a secondary runner instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerInstanceConfig {
    pub id: String,
    pub name: String,
    pub port: u16,
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

fn default_app_mode() -> String {
    "advanced".to_string()
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

pub fn get_container_settings() -> crate::container::container_config::ContainerConfig {
    let settings = load_settings();
    settings.container
}

pub fn save_container_settings(config: crate::container::container_config::ContainerConfig) -> Result<(), String> {
    let mut settings = load_settings();
    settings.container = config;
    save_settings(&settings)
}

/// Save the last loaded config path
pub fn save_last_config_path(path: &str) -> Result<(), String> {
    crate::config_facade::save_last_config_path(path)
}

/// Get the last loaded config path
pub fn get_last_config_path() -> Option<String> {
    crate::config_facade::get_last_config_path()
}

/// Check if setup wizard has been completed
pub fn get_setup_completed() -> bool {
    crate::config_facade::get_setup_completed()
}

/// Mark setup wizard as completed
pub fn save_setup_completed(completed: bool) -> Result<(), String> {
    crate::config_facade::save_setup_completed(completed)
}

/// Get the current debug settings
pub fn get_debug_settings() -> DebugSettings {
    crate::config_facade::get_setting::<DebugSettings>()
}

/// Save debug settings
pub fn save_debug_settings(debug_settings: DebugSettings) -> Result<(), String> {
    crate::config_facade::save_setting(debug_settings)
}

/// Save the last used workflow ID
pub fn save_last_workflow_id(workflow_id: &str) -> Result<(), String> {
    crate::config_facade::save_last_workflow_id(workflow_id)
}

/// Get the last used workflow ID
pub fn get_last_workflow_id() -> Option<String> {
    crate::config_facade::get_last_workflow_id()
}

/// Save the last used monitor index
pub fn save_last_monitor_index(monitor_index: i32) -> Result<(), String> {
    crate::config_facade::save_last_monitor_index(monitor_index)
}

/// Get the last used monitor index
pub fn get_last_monitor_index() -> Option<i32> {
    crate::config_facade::get_last_monitor_index()
}

/// Save the last used monitor indices (multi-monitor support)
pub fn save_last_monitor_indices(monitor_indices: Vec<i32>) -> Result<(), String> {
    crate::config_facade::save_last_monitor_indices(monitor_indices)
}

/// Get the last used monitor indices (multi-monitor support)
/// Falls back to legacy single monitor index if not set
pub fn get_last_monitor_indices() -> Option<Vec<i32>> {
    crate::config_facade::get_last_monitor_indices()
}

/// Get the auto-load last config setting
pub fn get_auto_load_last_config() -> bool {
    crate::config_facade::get_auto_load_last_config()
}

/// Save the auto-load last config setting
pub fn save_auto_load_last_config(enabled: bool) -> Result<(), String> {
    crate::config_facade::save_auto_load_last_config(enabled)
}

/// Get the configured Claude Code config directories
pub fn get_claude_config_dirs() -> Vec<String> {
    crate::config_facade::get_claude_config_dirs()
}

/// Save the configured Claude Code config directories
pub fn save_claude_config_dirs(dirs: Vec<String>) -> Result<(), String> {
    crate::config_facade::save_claude_config_dirs(dirs)
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
    crate::config_facade::get_interactive_sessions_enabled()
}

/// Save the interactive sessions enabled setting
pub fn save_interactive_sessions_enabled(enabled: bool) -> Result<(), String> {
    crate::config_facade::save_interactive_sessions_enabled(enabled)
}

/// Get the auto-continue AI workflow setting
pub fn get_auto_continue_ai_workflow() -> bool {
    crate::config_facade::get_auto_continue_ai_workflow()
}

/// Save the auto-continue AI workflow setting
pub fn save_auto_continue_ai_workflow(enabled: bool) -> Result<(), String> {
    crate::config_facade::save_auto_continue_ai_workflow(enabled)
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
    crate::config_facade::get_session_auto_fix_on_failure()
}

/// Save the session auto-fix on failure setting
#[allow(dead_code)]
pub fn save_session_auto_fix_on_failure(enabled: bool) -> Result<(), String> {
    crate::config_facade::save_session_auto_fix_on_failure(enabled)
}

/// Get the include summary step by default setting
pub fn get_include_summary_step_by_default() -> bool {
    crate::config_facade::get_include_summary_step_by_default()
}

/// Save the include summary step by default setting
pub fn save_include_summary_step_by_default(enabled: bool) -> Result<(), String> {
    crate::config_facade::save_include_summary_step_by_default(enabled)
}

/// Get the current Accessibility settings
pub fn get_accessibility_settings() -> AccessibilitySettings {
    crate::config_facade::get_setting::<AccessibilitySettings>()
}

/// Save Accessibility settings
pub fn save_accessibility_settings(
    accessibility_settings: AccessibilitySettings,
) -> Result<(), String> {
    crate::config_facade::save_setting(accessibility_settings)
}

/// Get the current Self-Healing settings
pub fn get_self_healing_settings() -> SelfHealingSettings {
    crate::config_facade::get_setting::<SelfHealingSettings>()
}

/// Save Self-Healing settings
pub fn save_self_healing_settings(
    self_healing_settings: SelfHealingSettings,
) -> Result<(), String> {
    crate::config_facade::save_setting(self_healing_settings)
}

// ============================================================================
// Path Settings
// ============================================================================

/// Get the current Path settings
pub fn get_path_settings() -> PathSettings {
    crate::config_facade::get_setting::<PathSettings>()
}

/// Get the dev_logs_dir override (used by paths module)
pub fn get_dev_logs_dir_override() -> Option<String> {
    crate::config_facade::get_dev_logs_dir_override()
}

/// Save Path settings
pub fn save_path_settings(path_settings: PathSettings) -> Result<(), String> {
    crate::config_facade::save_setting(path_settings)
}

/// Save the dev_logs_dir override
pub fn save_dev_logs_dir(dev_logs_dir: Option<String>) -> Result<(), String> {
    crate::config_facade::save_dev_logs_dir(dev_logs_dir)
}

// ============================================================================
// Mobile Settings
// ============================================================================

/// Get the current Mobile settings
pub fn get_mobile_settings() -> MobileSettings {
    crate::config_facade::get_setting::<MobileSettings>()
}

/// Save Mobile settings
pub fn save_mobile_settings(mobile_settings: MobileSettings) -> Result<(), String> {
    crate::config_facade::save_setting(mobile_settings)
}

// ============================================================================
// Global Log Source Settings
// ============================================================================

/// Get the current Global Log Source settings
pub fn get_global_log_source_settings() -> GlobalLogSourceSettings {
    crate::config_facade::get_setting::<GlobalLogSourceSettings>()
}

/// Save Global Log Source settings
pub fn save_global_log_source_settings(
    log_source_settings: GlobalLogSourceSettings,
) -> Result<(), String> {
    crate::config_facade::save_setting(log_source_settings)
}

/// Get enabled log sources, optionally filtered by profile
pub fn get_enabled_log_sources(profile_id: Option<&str>) -> Vec<GlobalLogSource> {
    crate::config_facade::get_enabled_log_sources(profile_id)
}

/// Seed default log sources if the sources list is empty.
pub fn seed_default_log_sources_if_empty() {
    crate::config_facade::seed_default_log_sources_if_empty()
}

/// Get log sources for AI selection prompt
pub fn get_log_sources_for_ai_selection() -> Vec<GlobalLogSource> {
    crate::config_facade::get_log_sources_for_ai_selection()
}

/// Get the AI selection mode
pub fn get_log_source_ai_selection_mode() -> LogSourceAiSelectionMode {
    crate::config_facade::get_log_source_ai_selection_mode()
}

// ============================================================================
// Managed Processes Settings
// ============================================================================

/// Get all managed process configurations
pub fn get_managed_process_configs() -> Vec<crate::process_capture::ProcessConfig> {
    crate::config_facade::get_managed_process_configs()
}

/// Save a managed process config (add or update).
pub fn save_managed_process_config(
    config: crate::process_capture::ProcessConfig,
) -> Result<(), String> {
    crate::config_facade::save_managed_process_config(config)
}

/// Delete a managed process config by ID.
pub fn delete_managed_process_config(id: &str) -> Result<(), String> {
    crate::config_facade::delete_managed_process_config(id)
}

// ============================================================================
// Execution Variables Settings
// ============================================================================

/// Get the current Execution Variables settings
pub fn get_execution_variables_settings() -> ExecutionVariablesSettings {
    crate::config_facade::get_setting::<ExecutionVariablesSettings>()
}

/// Save Execution Variables settings
pub fn save_execution_variables_settings(
    execution_variables_settings: ExecutionVariablesSettings,
) -> Result<(), String> {
    crate::config_facade::save_setting(execution_variables_settings)
}

/// Get the resolved auth token based on current settings
pub fn get_resolved_auth_token() -> Option<String> {
    crate::config_facade::get_resolved_auth_token()
}

/// Get resolved custom variables
pub fn get_resolved_custom_variables() -> HashMap<String, String> {
    crate::config_facade::get_resolved_custom_variables()
}

// ============================================================================
// Runner Instance CRUD Helpers
// ============================================================================

/// Get all configured runner instances.
pub fn get_runner_instances() -> Vec<RunnerInstanceConfig> {
    crate::config_facade::get_runner_instances()
}

/// Save or update a runner instance configuration.
pub fn save_runner_instance(config: RunnerInstanceConfig) -> Result<(), String> {
    crate::config_facade::save_runner_instance(config)
}

/// Delete a runner instance configuration by ID.
pub fn delete_runner_instance(id: &str) -> Result<(), String> {
    crate::config_facade::delete_runner_instance(id)
}

// ============================================================================
// OpenTelemetry Settings
// ============================================================================

/// Get the current OpenTelemetry settings.
pub fn get_otel_settings() -> crate::otel::OtelConfig {
    crate::config_facade::get_setting::<crate::otel::OtelConfig>()
}

/// Save OpenTelemetry settings.
/// Note: OTel cannot be hot-reloaded; changes take effect on next runner restart.
pub fn save_otel_settings(config: crate::otel::OtelConfig) -> Result<(), String> {
    crate::config_facade::save_setting(config)
}
