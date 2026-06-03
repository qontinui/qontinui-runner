use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{error, info};

use crate::ai_router::RoutingConfig;
use crate::orchestrator::{CompressionConfig, RetryConfig};

const SETTINGS_FILE: &str = "settings.json";

// ============================================================================
// Runner Tier
// ============================================================================

/// User tier selection — see plans/2026-05-20-runner-tier-decoupling.md.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RunnerTier {
    /// Tier 0 — local AI, no Qontinui account, no cloud round-trips.
    #[default]
    Local,
    /// Tier 1 — BYO API keys, no Qontinui account.
    LocalProvider,
    /// Tier 2 — signed into Qontinui; multi-machine coordination via the
    /// existing runner ↔ web WS bridge.
    QontinuiAccount,
}

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
    /// Local Ollama instance (Tier 0). Default endpoint: http://127.0.0.1:11434
    Ollama,
    /// Generic OpenAI-compatible HTTP endpoint (vLLM, Gemma, LM Studio, etc.).
    /// User supplies the base URL via `OpenAiCompatibleSettings.base_url`.
    OpenAiCompatible,
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
    Manual, // Use the explicitly configured config_dir
    #[default]
    LeastUsage, // Auto-select the account with lowest utilization. No-op when fewer than two config dirs are configured, so safe as the default.
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
            account_selection_mode: AccountSelectionMode::LeastUsage,
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

/// Settings for a local Ollama instance (Tier 0).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaSettings {
    /// Base URL of the Ollama HTTP API. Default: http://127.0.0.1:11434
    #[serde(default = "default_ollama_base_url")]
    pub base_url: String,
    /// Model name (e.g. "llama3.1:8b").
    #[serde(default = "default_ollama_provider_model")]
    pub model: String,
    #[serde(default = "default_ollama_timeout_secs")]
    pub timeout_seconds: u64,
}

fn default_ollama_base_url() -> String {
    "http://127.0.0.1:11434".to_string()
}
fn default_ollama_provider_model() -> String {
    "llama3.1:8b".to_string()
}
fn default_ollama_timeout_secs() -> u64 {
    600
}

impl Default for OllamaSettings {
    fn default() -> Self {
        Self {
            base_url: default_ollama_base_url(),
            model: default_ollama_provider_model(),
            timeout_seconds: default_ollama_timeout_secs(),
        }
    }
}

/// Settings for any OpenAI-compatible HTTP endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleSettings {
    /// Full base URL — e.g. "http://localhost:8080/v1" for a vLLM server.
    #[serde(default)]
    pub base_url: String,
    /// Model identifier the server expects.
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_openai_compatible_timeout")]
    pub timeout_seconds: u64,
    // Note: API key (if any) stored separately in OS keychain via
    // `ai_keychain().store("openai_compatible", ...)`.
}

fn default_openai_compatible_timeout() -> u64 {
    600
}

impl Default for OpenAiCompatibleSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            model: String::new(),
            timeout_seconds: default_openai_compatible_timeout(),
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
    /// Local Ollama instance settings (Tier 0).
    #[serde(default)]
    pub ollama: OllamaSettings,
    /// Generic OpenAI-compatible endpoint settings (vLLM, LM Studio, etc.).
    #[serde(default)]
    pub openai_compatible: OpenAiCompatibleSettings,
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
    /// Send launch prompts to Claude (Haiku 4.5) for path inference in
    /// the predictive-conflict-warning probe. When false, the AI extractor
    /// is skipped and only the deterministic regex extractor runs. The
    /// regex extractor is always on regardless of this flag.
    #[serde(default = "default_ai_path_prediction_enabled")]
    pub ai_path_prediction_enabled: bool,
    /// Federate Claude memory writes through coord. When true (default),
    /// each spawned Claude session pulls the tenant memory pool before
    /// spawn, runs a file watcher during the session that pushes
    /// changes to coord, and reconciles on session end. When false,
    /// each session is local-only (the per-account memory dir is
    /// untouched by the runner). UI surface for this is deferred —
    /// flip via direct settings edit.
    #[serde(default = "default_memory_federation_enabled")]
    pub memory_federation_enabled: bool,
}

fn default_interactive_sessions_enabled() -> bool {
    true
}

fn default_ai_path_prediction_enabled() -> bool {
    true
}

fn default_memory_federation_enabled() -> bool {
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
            ollama: OllamaSettings::default(),
            openai_compatible: OpenAiCompatibleSettings::default(),
            auto_refine_video_after_iterations: default_auto_refine_video_after_iterations(),
            compression: CompressionConfig::default(),
            retry: RetryConfig::default(),
            routing: RoutingConfig::default(),
            interactive_sessions_enabled: default_interactive_sessions_enabled(),
            ai_path_prediction_enabled: default_ai_path_prediction_enabled(),
            memory_federation_enabled: default_memory_federation_enabled(),
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
    /// Use Rust-native accessibility APIs (UIA/AT-SPI/AX) instead of Python HAL backends.
    /// When enabled, the runner captures accessibility trees directly via platform APIs
    /// without crossing the Python bridge, providing faster and more reliable automation.
    #[serde(default = "default_use_rust_accessibility")]
    pub use_rust_accessibility: bool,
}

fn default_cdp_port() -> u16 {
    9222
}

fn default_use_rust_accessibility() -> bool {
    true
}

impl Default for AccessibilitySettings {
    fn default() -> Self {
        Self {
            chrome_path: None, // Auto-detect
            cdp_port: default_cdp_port(),
            use_rust_accessibility: default_use_rust_accessibility(),
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

    /// Default remote port that UI Bridge listens on mobile devices (default: 8087)
    #[serde(default = "default_ui_bridge_port")]
    pub ui_bridge_port: u16,

    /// Enable LAN (Wi-Fi) discovery via mDNS
    #[serde(default = "default_true")]
    pub lan_discovery_enabled: bool,

    /// Allow plain HTTP for LAN connections (default: false, requires TLS)
    #[serde(default)]
    pub lan_allow_plaintext: bool,

    /// Pairing timeout in seconds (default: 600)
    #[serde(default = "default_pairing_timeout")]
    pub pairing_timeout_secs: u64,

    /// Known/paired physical devices
    #[serde(default)]
    pub paired_devices: Vec<crate::mcp::transport::pairing::PairedDeviceRecord>,
}

fn default_logcat_lines() -> u32 {
    500
}

fn default_ui_bridge_port() -> u16 {
    8087
}

fn default_pairing_timeout() -> u64 {
    600
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
            ui_bridge_port: default_ui_bridge_port(),
            lan_discovery_enabled: default_true(),
            lan_allow_plaintext: false,
            pairing_timeout_secs: default_pairing_timeout(),
            paired_devices: Vec::new(),
        }
    }
}

// ============================================================================
// Tunnel Settings (Plan 1B)
// ============================================================================

/// Settings for the rathole-based reverse tunnel that lets off-network devices
/// reach the runner. Replaces the ephemeral Cloudflare quick-tunnel.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TunnelSettings {
    /// Whether tunnel wiring is allowed at all.
    #[serde(default)]
    pub enabled: bool,

    /// Rathole server address, e.g. `"relay.qontinui.io:2333"`.
    #[serde(default)]
    pub server_addr: String,

    /// Shared secret with the rathole server. Per-service tokens inherit this.
    #[serde(default)]
    pub default_token: Option<String>,

    /// If true, start the rathole client on runner boot when `enabled`.
    #[serde(default)]
    pub auto_connect: bool,
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
    /// Override example: `D:\qontinui-root\.dev-logs`
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
// Process Management Settings
// ============================================================================

/// Policy settings for managed-process lifecycle behaviour.
///
/// These knobs let operators tune how the runner handles externally-started
/// processes (e.g. a dev server started outside the runner) without editing
/// code or recompiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessManagementSettings {
    /// When `true` (default) and the runner is in dev-mode
    /// (`dev_services::is_dev_mode()`), the reconcile loop will adopt a
    /// foreign process that is already bound to a configured health port
    /// (transitioning the config's state to `ExternallyOwned`).
    ///
    /// Setting this to `false` forces the "refuse / kill" posture even in
    /// dev: the port owner is killed before the runner spawns its own copy,
    /// exactly as in production.
    #[serde(default = "default_external_adoption_in_dev")]
    pub external_adoption_in_dev: bool,
}

fn default_external_adoption_in_dev() -> bool {
    true
}

impl Default for ProcessManagementSettings {
    fn default() -> Self {
        Self {
            external_adoption_in_dev: default_external_adoption_in_dev(),
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
// Web Integration Settings (Phase 3G)
// ============================================================================

/// Web-integration (qontinui-web backend) settings.
///
/// Controls whether this runner registers with the web backend, sends
/// heartbeats, and posts phase-completion events. Decoupled from the
/// `QONTINUI_SERVER_MODE` env var so any runner (primary, secondary, or
/// headless) can enable web integration independently of headless-window
/// behavior. Env vars `QONTINUI_WEB_BACKEND_URL` + `QONTINUI_RUNNER_TOKEN`
/// still work as runtime-only overrides for headless deploys.
///
/// Defaults are tuned for a fresh install to be visible to the qontinui-web
/// `/connect` flow without any manual configuration: `enabled = true` and
/// `backend_url` points at the dev backend in debug builds (`http://localhost:8000`)
/// or the production backend in release builds (`https://api.qontinui.io`).
/// `runner_token` still defaults to empty — it must be granted by the user
/// through one of the device-pairing flows (Cognito sign-in or pair-code
/// redemption). The runner UI surfaces a "needs authorization" banner
/// whenever `enabled && runner_token.is_empty()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebIntegrationSettings {
    /// Master toggle. When false, the runner does not register, heartbeat, or emit events.
    #[serde(default = "default_web_integration_enabled")]
    pub enabled: bool,

    /// API base — the FastAPI backend that serves `/api/v1/*`. In a unified
    /// production deployment this is also the origin that serves the web SPA.
    /// Example: `https://api.qontinui.io` or `http://127.0.0.1:8000`.
    /// No trailing slash required.
    #[serde(default = "default_web_integration_backend_url")]
    pub backend_url: String,

    /// Optional override for the Next.js web frontend origin — the host that
    /// serves user-facing pages like `/connect-runner`. When unset, falls
    /// back to `backend_url`. Only needed in split local-dev setups where
    /// the API (8000) and SPA (3001) run on different ports.
    /// No trailing slash required.
    #[serde(default)]
    pub web_base_url: Option<String>,

    /// Long-lived runner-side bearer (qontinui_runner_<64hex>) minted by
    /// web's `/connect-runner` flow and exchanged for a coord-issued
    /// device-JWT by `mcp::device_jwt_refresher`. Phase 3 of the
    /// unified-devices migration removed this from the WS handshake; it
    /// is now presented ONLY on outbound HTTP to `<coord>/coord/devices/
    /// pair-cli` as the OAuth-style bearer. The relay's `Authorization:
    /// Bearer` header comes from `AuthManager`'s access_token slot (the
    /// device-JWT) instead.
    ///
    /// Stored plaintext — acceptable since the settings file is user-local.
    /// The device-JWT itself lives in `AuthManager`'s encrypted file
    /// storage (`auth_tokens.enc`).
    #[serde(default)]
    pub runner_token: String,
}

/// Default for [`WebIntegrationSettings::enabled`] — `true` so a fresh install
/// is connected to its backend out of the box. The user can opt out via
/// Settings → Web Integration; we still respect that choice when set.
fn default_web_integration_enabled() -> bool {
    true
}

/// Default for [`WebIntegrationSettings::backend_url`] — the dev backend in
/// debug builds, the production backend in release builds. We pick this at
/// compile time via `cfg(debug_assertions)` to match how other defaults in
/// this codebase distinguish dev vs prod (see `dev_services.rs`).
///
/// The user can override either value via Settings → Web Integration; the
/// override is persisted to `settings.json` and survives upgrades.
pub(crate) fn default_web_integration_backend_url() -> String {
    if cfg!(debug_assertions) {
        "http://localhost:8000".to_string()
    } else {
        crate::api_config::PROD_API_BASE_URL.to_string()
    }
}

impl Default for WebIntegrationSettings {
    fn default() -> Self {
        Self {
            enabled: default_web_integration_enabled(),
            backend_url: default_web_integration_backend_url(),
            web_base_url: None,
            runner_token: String::new(),
        }
    }
}

#[cfg(test)]
mod web_integration_default_tests {
    use super::*;

    /// A fresh install (no persisted settings) must default to:
    ///   - enabled: true
    ///   - backend_url: dev or prod URL depending on build profile
    ///   - runner_token: empty (token requires user OAuth consent)
    ///   - web_base_url: None
    ///
    /// This locks in the "fresh install is connected by default" contract:
    /// when these defaults change, the `/connect` flow on the mobile app
    /// breaks and the in-runner authorization banner stops appearing.
    #[test]
    fn default_is_enabled_with_environment_appropriate_backend_url() {
        let s = WebIntegrationSettings::default();
        assert!(
            s.enabled,
            "fresh install must default to enabled — disabled fresh installs are invisible to /connect"
        );
        assert!(
            s.runner_token.is_empty(),
            "runner_token must default to empty — token comes from user OAuth consent only"
        );
        assert_eq!(s.web_base_url, None);

        if cfg!(debug_assertions) {
            assert_eq!(s.backend_url, "http://localhost:8000");
        } else {
            assert_eq!(s.backend_url, "https://api.qontinui.io");
        }
    }

    /// An empty JSON object must deserialize to the same defaults as
    /// `WebIntegrationSettings::default()`. This guarantees that a runner
    /// upgrading from a settings.json missing the `web_integration` key —
    /// or with `web_integration: {}` — also picks up the new defaults.
    #[test]
    fn deserializes_empty_object_to_default() {
        let parsed: WebIntegrationSettings =
            serde_json::from_str("{}").expect("empty object must deserialize");
        assert_eq!(parsed, WebIntegrationSettings::default());
    }

    /// User-provided values must take precedence over the new defaults —
    /// this is the upgrade-safety contract. A user who explicitly disabled
    /// web integration before the default flipped to `true` keeps their
    /// `enabled: false`. Same for a custom backend URL.
    #[test]
    fn explicit_user_values_override_defaults() {
        let parsed: WebIntegrationSettings =
            serde_json::from_str(r#"{"enabled": false, "backend_url": "http://my-backend:9999"}"#)
                .expect("must deserialize");
        assert!(!parsed.enabled);
        assert_eq!(parsed.backend_url, "http://my-backend:9999");
    }
}

#[cfg(test)]
mod tier_tests {
    use super::*;

    #[test]
    fn fresh_settings_default_to_tier_local() {
        let s = Settings::default();
        assert_eq!(s.tier, RunnerTier::Local);
        assert!(s.qontinui_user_id.is_none());
    }

    #[test]
    fn deserializes_missing_tier_field_to_local_default() {
        let parsed: Settings = serde_json::from_str("{}").expect("empty object must deserialize");
        assert_eq!(parsed.tier, RunnerTier::Local);
        assert!(!parsed.tier_initialized);
        assert!(parsed.local_user_id.is_empty());
        assert!(parsed.qontinui_user_id.is_none());
    }

    /// Tier-inference: a settings.json with a runner_token but no tier
    /// (the upgrade-from-pre-tier shape) must land in QontinuiAccount on
    /// first load, and the sentinel must flip true.
    #[test]
    fn migrate_tier_from_runner_token_infers_qontinui_account() {
        let mut s = Settings {
            tier_initialized: false,
            tier: RunnerTier::Local, // pre-migration in-memory state
            web_integration: WebIntegrationSettings {
                runner_token: "qontinui_runner_abc".to_string(),
                ..WebIntegrationSettings::default()
            },
            ..Settings::default()
        };

        let migrated = migrate_tier_in_place(&mut s);
        assert!(migrated, "must report migration performed");
        assert_eq!(s.tier, RunnerTier::QontinuiAccount);
        assert!(s.tier_initialized);
    }

    /// Tier-inference: a settings.json with no runner_token and no tier
    /// (genuinely fresh install) must land in Local with the sentinel set.
    #[test]
    fn migrate_tier_without_runner_token_stays_local() {
        let mut s = Settings {
            tier_initialized: false,
            ..Settings::default()
        };
        s.web_integration.runner_token.clear();

        let migrated = migrate_tier_in_place(&mut s);
        assert!(migrated);
        assert_eq!(s.tier, RunnerTier::Local);
        assert!(s.tier_initialized);
    }

    /// Tier-inference must be a one-shot: once `tier_initialized` is true,
    /// subsequent loads must not overwrite a deliberate user tier choice.
    #[test]
    fn migrate_tier_is_no_op_once_initialized() {
        let mut s = Settings {
            tier_initialized: true,
            tier: RunnerTier::LocalProvider, // user explicitly chose Tier 1
            web_integration: WebIntegrationSettings {
                runner_token: "qontinui_runner_xyz".to_string(),
                ..WebIntegrationSettings::default()
            },
            ..Settings::default()
        };

        let migrated = migrate_tier_in_place(&mut s);
        assert!(!migrated, "must not re-migrate when initialized");
        assert_eq!(s.tier, RunnerTier::LocalProvider);
    }

    #[test]
    fn tier_serializes_snake_case() {
        let json = serde_json::to_string(&RunnerTier::QontinuiAccount).unwrap();
        assert_eq!(json, "\"qontinui_account\"");
        let json = serde_json::to_string(&RunnerTier::LocalProvider).unwrap();
        assert_eq!(json, "\"local_provider\"");
        let json = serde_json::to_string(&RunnerTier::Local).unwrap();
        assert_eq!(json, "\"local\"");
    }

    /// Env-var overlay must override a migrated Tier 2 down to Local — the
    /// supervisor-spawned-temp-runner scenario where the shared settings.json
    /// already inferred QontinuiAccount from the primary's `runner_token`.
    #[test]
    fn tier_env_overlay_local_demotes_qontinui_account() {
        let mut s = Settings {
            tier_initialized: true,
            tier: RunnerTier::QontinuiAccount,
            web_integration: WebIntegrationSettings {
                runner_token: "qontinui_runner_primary".to_string(),
                ..WebIntegrationSettings::default()
            },
            ..Settings::default()
        };
        apply_tier_env_overlay(&mut s, "local");
        assert_eq!(s.tier, RunnerTier::Local);
    }

    #[test]
    fn tier_env_overlay_accepts_all_three_values() {
        let mut s = Settings::default();
        apply_tier_env_overlay(&mut s, "qontinui_account");
        assert_eq!(s.tier, RunnerTier::QontinuiAccount);
        apply_tier_env_overlay(&mut s, "local_provider");
        assert_eq!(s.tier, RunnerTier::LocalProvider);
        apply_tier_env_overlay(&mut s, "local");
        assert_eq!(s.tier, RunnerTier::Local);
    }

    #[test]
    fn tier_env_overlay_is_case_and_whitespace_tolerant() {
        let mut s = Settings {
            tier: RunnerTier::QontinuiAccount,
            ..Settings::default()
        };
        apply_tier_env_overlay(&mut s, "  Local  ");
        assert_eq!(s.tier, RunnerTier::Local);
    }

    /// An unrecognized value must leave the persisted tier untouched. The
    /// supervisor should never send a bad value, but a typo in a CI script
    /// shouldn't silently demote a Tier 2 runner.
    #[test]
    fn tier_env_overlay_unknown_value_preserves_tier() {
        let mut s = Settings {
            tier: RunnerTier::QontinuiAccount,
            ..Settings::default()
        };
        apply_tier_env_overlay(&mut s, "nope");
        assert_eq!(s.tier, RunnerTier::QontinuiAccount);
    }

    /// FOOTGUN GUARD: a supervisor-spawned temp/named runner (secondary) must
    /// NEVER persist a tier/local_user_id migration to the shared
    /// settings.json — doing so would infer `tier=Local` (no runner_token) and
    /// clobber the primary's persisted Tier 2 state. Only the primary may
    /// persist. `should_persist_migration` encodes that decision.
    #[test]
    fn secondary_runner_must_not_persist_migration() {
        // Secondary with a pending migration: persist is suppressed.
        assert!(
            !should_persist_migration(true, /* is_secondary = */ true),
            "a secondary runner must never persist a migration to the shared settings.json"
        );
        // Primary with a pending migration: persist proceeds.
        assert!(
            should_persist_migration(true, /* is_secondary = */ false),
            "the primary runner must persist its tier/local_user_id migration"
        );
        // Nothing to persist: never persist, regardless of runner kind.
        assert!(!should_persist_migration(false, false));
        assert!(!should_persist_migration(false, true));
    }
}

// ============================================================================
// Cloud Relay Settings
// ============================================================================

// TODO: `CloudRelaySettings` is now only used by the cloud device-bridge
// poller in `mcp::discovery::cloud_registry` / `mcp::transport::cloud`
// (mobile-tunnel feature). The runner ↔ web channel is driven entirely by
// `WebIntegrationSettings`. If the device-bridge feature is retired, drop
// this struct and inline a minimal `mobile_tunnel` settings object.
/// Cloud relay settings — retained for the cloud device-bridge poller used
/// by the mobile-tunnel feature. The runner ↔ web channel is driven by
/// `WebIntegrationSettings`, not this struct.
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
    /// Enable device bridging via cloud relay
    #[serde(default)]
    pub device_bridge_enabled: bool,
    /// Poll interval for cloud device registry in seconds (default: 30)
    #[serde(default = "default_cloud_poll_secs")]
    pub cloud_registry_poll_secs: u64,
}

fn default_backend_url() -> String {
    "https://qontinui.io".to_string()
}

fn default_cloud_poll_secs() -> u64 {
    30
}

impl Default for CloudRelaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            backend_url: default_backend_url(),
            auto_connect: false,
            device_bridge_enabled: false,
            cloud_registry_poll_secs: default_cloud_poll_secs(),
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

fn default_min_group_size() -> usize {
    3
}
fn default_cooldown_hours() -> f64 {
    6.0
}
fn default_archive_threshold() -> f64 {
    0.05
}
fn default_max_observations() -> i64 {
    500
}

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
    /// Policy settings for managed-process lifecycle (adoption, kill posture, etc.).
    #[serde(default)]
    pub process_management: ProcessManagementSettings,
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
    /// Rathole reverse-tunnel settings (Plan 1B, replaces Cloudflare quick-tunnel)
    #[serde(default)]
    pub tunnel: TunnelSettings,
    /// Configured runner instances for multi-instance dev workflows
    #[serde(default)]
    pub runner_instances: Vec<RunnerInstanceConfig>,
    /// Temp-runner spawn placements. Distinct from per-named-instance placements
    /// in `runner_instances[i].spawn_placement` — these only apply to runners the
    /// supervisor spawns via `POST /runners/spawn-test`. Round-robin'd across the
    /// list at supervisor request time. Empty = supervisor falls back to OS
    /// default placement (current pre-feature behavior).
    #[serde(default)]
    pub temp_spawn_placements: Vec<SpawnPlacement>,
    /// Custom desktop ports to scan for UI Bridge endpoints (merged with defaults).
    #[serde(default)]
    pub discovery_ports: Vec<u16>,
    /// Directories containing Claude Code configs (each should have a `projects/` subdirectory).
    /// Used by Terminal > Browse Sessions to find transcript files.
    #[serde(default)]
    pub claude_config_dirs: Vec<String>,
    /// Custom launch commands per account config directory.
    /// Key: config_dir path, Value: shell command (e.g. "clg" instead of default CLAUDE_CONFIG_DIR pattern).
    #[serde(default)]
    pub claude_account_launch_commands: std::collections::HashMap<String, String>,
    /// OpenTelemetry configuration for optional OTLP trace export.
    /// Note: OTel cannot be hot-reloaded; changes require a runner restart.
    #[serde(default)]
    pub otel: crate::otel::OtelConfig,
    #[serde(default)]
    pub container: crate::container::container_config::ContainerConfig,
    /// Security and sandbox configuration (profiles, policies, audit settings).
    #[serde(default)]
    pub security: crate::security::engine::SecuritySettings,
    /// Memory consolidation settings (importance decay, grouping, LLM model)
    #[serde(default)]
    pub memory_consolidation: MemoryConsolidationSettings,
    /// Restate durable execution settings (journal replay, exactly-once, saga compensation)
    #[serde(default)]
    pub restate: crate::restate::config::RestateSettings,
    /// World State Verifier (CUA-WSM / SEAgent judge) configuration.
    /// Controls whether the runtime agentic loop consults a VLM judge
    /// to compare pre/post screenshots against declared intent.
    #[serde(default)]
    pub world_state_verifier: WorldStateVerifierSettings,
    /// Scripted-output (think-in-code) emitter kill switch and model
    /// override. When `enabled` is false, or the `QONTINUI_SCRIPTED_OUTPUT`
    /// env var is not "1", the TS `ScriptedOutputHandler` falls back to
    /// plain truncation. See `step_output::script_emitter`.
    #[serde(default)]
    pub scripted_output: crate::step_output::script_emitter::ScriptedOutputSettings,
    /// Web-integration (qontinui-web backend) configuration. When
    /// `web_integration.enabled` is true and both `backend_url` + `runner_token`
    /// are set, the runner registers with web, sends heartbeats, and posts
    /// phase events — independent of `QONTINUI_SERVER_MODE` (which now only
    /// controls headless-window behavior).
    #[serde(default)]
    pub web_integration: WebIntegrationSettings,
    /// User-curated list of projects. Populated primarily from the setup
    /// wizard's project-picker step and surfaced to other UI (e.g. the UI
    /// Bridge Integration panel) as a dropdown of known project paths.
    /// Back-compat: missing key in an existing settings.json loads as empty.
    #[serde(default)]
    pub saved_projects: Vec<SavedProject>,
    /// Trace API gate (Section 5b of the UI Bridge redesign). When enabled,
    /// `/trace/...` routes are mounted on the runner's HTTP API and the
    /// runner persists causal traces to `project.ui_bridge_events` (requires
    /// Alembic migration `section_5b_01_ui_bridge_causal_columns`).
    /// Default: false (shipped but inert until the migration is applied).
    #[serde(default)]
    pub trace_api: TraceApiSettings,
    /// Auto-yield-on-idle policy for file locks. See
    /// `executor::auto_yield_policy` for the background task that
    /// consumes this setting. Defaults preserve historical behavior:
    /// `enabled = false`, so existing deployments don't auto-yield
    /// until a user opts in.
    #[serde(default)]
    pub lock_yield_policy: LockYieldPolicySettings,
    /// User tier — controls whether the runner reaches qontinui-web for auth.
    /// Inferred from `web_integration.runner_token` on first load if missing
    /// (see `load_settings` post-processing). Default for genuinely fresh
    /// installs: Local.
    #[serde(default)]
    pub tier: RunnerTier,
    /// Set true once `load_settings` has inferred a tier value from prior
    /// settings (or confirmed there was nothing to infer). Used as a
    /// one-shot migration sentinel so upgraders with a `runner_token`
    /// already populated land in `QontinuiAccount` exactly once.
    #[serde(default)]
    pub tier_initialized: bool,
    /// Per-`~/.qontinui/`-dir UUID identifying this install for local-DB rows.
    /// Populated lazily by `load_settings` when empty. Persists across Tier
    /// upgrades and Tier-2 sign-outs — never replaced by the Qontinui user id.
    #[serde(default)]
    pub local_user_id: String,
    /// Qontinui user id (from the access token's `sub` claim). Filled on
    /// Tier-2 sign-in, cleared on sign-out. `local_user_id` stays alongside.
    #[serde(default)]
    pub qontinui_user_id: Option<String>,
}

// ============================================================================
// Trace API Settings (Section 5b)
// ============================================================================

/// Trace API gate. When `enabled` is false, the `/trace/...` routes are not
/// mounted and no causal-trace DB writes are issued. The default is `false` —
/// matches the "shipped but inert" rollout pattern used elsewhere in the
/// redesign.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceApiSettings {
    pub enabled: bool,
}

// ============================================================================
// Lock Yield Policy Settings (lock-yield-protocol-plan §Open Q4)
// ============================================================================

/// Auto-yield-on-idle policy for `FileLockManager`.
///
/// When `enabled`, a background task (see
/// `executor::auto_yield_policy`) periodically scans currently-held
/// file locks and, for each holder that has been idle (no terminal
/// stdout activity) for at least `idle_threshold_secs` AND whose
/// oldest waiter has been blocked for at least `min_wait_secs`,
/// releases the lock on the holder's behalf and emits a
/// `file-lock-auto-yielded` event.
///
/// Defaults are intentionally conservative: `enabled = false` so the
/// policy is opt-in. Once a user opts in, the 60s idle / 30s wait
/// thresholds keep transient contention (e.g. a sub-second multi-edit
/// burst from a busy session) from triggering false-positive yields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockYieldPolicySettings {
    /// Enable auto-yield. Default off until users opt in.
    #[serde(default)]
    pub enabled: bool,
    /// Minimum holder-idle duration (no stdout activity from the holding
    /// session for this many seconds) before the holder is considered
    /// yieldable. Default 60.
    #[serde(default = "default_auto_yield_idle_threshold_secs")]
    pub idle_threshold_secs: u64,
    /// Minimum wait duration (a waiter has been blocked on this file
    /// for at least this many seconds) before auto-yield fires.
    /// Default 30.
    #[serde(default = "default_auto_yield_min_wait_secs")]
    pub min_wait_secs: u64,
}

fn default_auto_yield_idle_threshold_secs() -> u64 {
    60
}
fn default_auto_yield_min_wait_secs() -> u64 {
    30
}

impl Default for LockYieldPolicySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_threshold_secs: default_auto_yield_idle_threshold_secs(),
            min_wait_secs: default_auto_yield_min_wait_secs(),
        }
    }
}

// ============================================================================
// Saved Projects (user-curated project registry)
// ============================================================================

/// A project the user has told the runner about (typically via the setup
/// wizard's project picker). Persisted to `settings.json` under the
/// `saved_projects` key.
///
/// Serialized as camelCase on the wire so JS/TS consumers (wizard, UI Bridge
/// Integration panel) can bind directly. The struct is deliberately loose —
/// `project_type` is a free-form string so new frameworks do not require a
/// schema change.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedProject {
    /// Absolute path to the project root.
    pub path: String,
    /// Human-friendly display name (usually the directory basename).
    pub name: String,
    /// Framework/language tag, e.g. "react", "python", "rust", "node".
    /// Kept loose (String) so future frameworks need no schema change.
    pub project_type: String,
    /// Manifest file that identified the project (e.g. "package.json").
    pub manifest: String,
}

// ============================================================================
// World State Verifier Settings
// ============================================================================

/// Tri-state mode for the World State Verifier.
///
/// - `Disabled`: WSM never runs; the text verifier agent is the sole path.
/// - `Enabled`: WSM runs first; its verdict wins when successful, falling
///   back to the text verifier on error.
/// - `Shadow`: Both run; text verifier decides, WSM disagreements are logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum WsvMode {
    #[default]
    Disabled,
    Enabled,
    Shadow,
}

/// World State Verifier settings persisted to the settings file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldStateVerifierSettings {
    /// Tri-state mode selector.
    #[serde(default)]
    pub mode: WsvMode,
    /// llama-swap (or compatible) endpoint base URL.
    #[serde(default = "default_wsv_endpoint")]
    pub endpoint: String,
    /// Model alias or HuggingFace id to request.
    #[serde(default = "default_wsv_model")]
    pub model: String,
    /// When true, append downsampled pre/post thumbnails to agentic
    /// iteration canvas panels. Phase 3 of the WSV UI rollout.
    #[serde(default = "default_wsv_show_screenshot_evidence")]
    pub show_screenshot_evidence: bool,
    /// True once the user has saved these settings at least once via
    /// the Settings UI. Used at startup to distinguish "never
    /// configured" from "explicitly saved as Disabled" so the env var
    /// is honored only when the user hasn't expressed an opinion yet.
    #[serde(default)]
    pub ever_saved: bool,
}

fn default_wsv_endpoint() -> String {
    "http://127.0.0.1:8100".to_string()
}

fn default_wsv_model() -> String {
    "cua-wsm".to_string()
}

fn default_wsv_show_screenshot_evidence() -> bool {
    true
}

impl Default for WorldStateVerifierSettings {
    fn default() -> Self {
        Self {
            mode: WsvMode::default(),
            endpoint: default_wsv_endpoint(),
            model: default_wsv_model(),
            show_screenshot_evidence: true,
            ever_saved: false,
        }
    }
}

// ============================================================================
// Runner Instance Configuration (Dev Feature)
// ============================================================================

/// Describes a target monitor for spawn placement. The serde tag is
/// `kind` and matches the discriminator in the JSON written to
/// `settings.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MonitorDescriptor {
    /// 0-based index into Tauri's `available_monitors()` list.
    Index { index: usize },
    /// Spatial role: "primary", "left", "right", "center".
    /// Resolved against the same labeling logic the runner uses
    /// in `mcp::monitors::get_monitors`.
    Position { position: String },
    /// Match a monitor by its OS name (e.g. `\\.\DISPLAY1`).
    Name { name: String },
}

/// Per-instance spawn-window placement, configured by the user and
/// resolved at launch time to absolute virtual-desktop physical coords.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPlacement {
    pub monitor: MonitorDescriptor,
    /// Position in monitor-local logical CSS pixels. (0, 0) is the
    /// monitor's top-left.
    pub x: i32,
    pub y: i32,
    /// Window size in monitor-local logical CSS pixels. Falls back to
    /// 1920x1080 when None.
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Show window decorations (title bar, borders, resize handles).
    /// Default true. Set false for borderless windows that sit flush
    /// with the monitor's edge — useful when the few-pixel window-border
    /// inset matters and the user doesn't need to drag/resize the window.
    #[serde(default)]
    pub decorations: Option<bool>,
}

/// Configuration for a secondary runner instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerInstanceConfig {
    pub id: String,
    pub name: String,
    pub port: u16,
    /// Optional per-instance spawn-window placement. None = let the OS
    /// place it (current behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn_placement: Option<SpawnPlacement>,
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
    let app_data_dir = std::env::var("QONTINUI_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("com.qontinui.runner")))
        .ok_or("Failed to get config directory")?;

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok(app_data_dir.join(SETTINGS_FILE))
}

/// Load settings from file
pub fn load_settings() -> Settings {
    let mut settings = match get_settings_path() {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<Settings>(&contents) {
                        Ok(s) => s,
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
    };

    // Supervisor-injected Restate port/URL overrides (Phase 2 plumbing).
    // These apply only to the in-memory settings for the current process;
    // they are never saved back to the JSON file.
    if let Some(p) = std::env::var("QONTINUI_RESTATE_INGRESS_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        settings.restate.ingress_port = p;
    }
    if let Some(p) = std::env::var("QONTINUI_RESTATE_ADMIN_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        settings.restate.admin_port = p;
    }
    if let Some(p) = std::env::var("QONTINUI_RESTATE_SERVICE_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        settings.restate.service_endpoint_port = p;
    }
    if let Ok(u) = std::env::var("QONTINUI_RESTATE_EXTERNAL_ADMIN_URL") {
        settings.restate.external_admin_url = Some(u);
    }
    if let Ok(u) = std::env::var("QONTINUI_RESTATE_EXTERNAL_INGRESS_URL") {
        settings.restate.external_ingress_url = Some(u);
    }

    // Web-integration env-var overrides (Phase 3G).
    // In-memory overlay only; never persisted to disk. If either variable
    // is set via env and a non-empty value is present in settings, the env
    // wins. If both env vars are set and the persisted `enabled` flag is
    // false, default to enabled (headless deploys shouldn't have to save
    // settings to activate web integration).
    let env_backend_url = std::env::var("QONTINUI_WEB_BACKEND_URL").ok();
    let env_runner_token = std::env::var("QONTINUI_RUNNER_TOKEN").ok();
    if let Some(v) = env_backend_url.as_ref() {
        if !v.is_empty() {
            settings.web_integration.backend_url = v.clone();
        }
    }
    if let Some(v) = env_runner_token.as_ref() {
        if !v.is_empty() {
            settings.web_integration.runner_token = v.clone();
        }
    }
    let has_env_pair = env_backend_url
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false)
        && env_runner_token
            .as_deref()
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    if has_env_pair && !settings.web_integration.enabled {
        settings.web_integration.enabled = true;
    }

    // Tier + local_user_id migration / lazy init.
    //
    // Both branches may mutate the in-memory `settings` and request a
    // persist. The persist is best-effort (logged on failure) — an in-memory
    // value is still correct for the rest of this process's lifetime.
    //
    // FOOTGUN GUARD: the persist below writes the SHARED settings.json
    // (`dirs::config_dir()/com.qontinui.runner/settings.json` — the same file
    // for primary + temp + named runners; `get_settings_path` is NOT
    // instance-scoped). A supervisor-spawned temp/named runner has no
    // `runner_token`, so `migrate_tier_in_place` infers `tier=Local` for it.
    // If that secondary persisted, it would silently overwrite the primary's
    // persisted Tier 2 (`qontinui_account`) state on disk, demoting the primary
    // the next time it loads from `local`. Therefore: ONLY the primary runner
    // may persist a tier/local_user_id migration. Secondaries (temp + named,
    // i.e. any runner the supervisor launched with `QONTINUI_INSTANCE_NAME`)
    // keep the migration in-memory only — correct for this process's lifetime,
    // never written to the shared file. This mirrors the in-memory-only
    // `QONTINUI_RUNNER_TIER` overlay just below.
    let mut needs_persist = false;
    if migrate_tier_in_place(&mut settings) {
        needs_persist = true;
    }
    if settings.local_user_id.trim().is_empty() {
        settings.local_user_id = uuid::Uuid::new_v4().to_string();
        needs_persist = true;
    }
    let is_secondary = crate::instance::is_secondary();
    if needs_persist && is_secondary {
        info!(
            "Skipping tier/local_user_id migration persist for secondary runner \
             (instance={:?}) — would clobber the primary's shared settings.json; \
             keeping the migration in-memory only (tier={:?})",
            crate::instance::instance_name(),
            settings.tier
        );
    }
    if should_persist_migration(needs_persist, is_secondary) {
        if let Err(e) = save_settings(&settings) {
            error!("Failed to persist tier/local_user_id migration: {}", e);
        } else {
            info!(
                "Persisted tier/local_user_id migration (tier={:?}, local_user_id set)",
                settings.tier
            );
        }
    }

    // Tier env-var overlay. In-memory only; never persisted. Lets a parent
    // process (notably the supervisor spawning a temp runner) force the
    // booted runner onto a specific tier without writing to the shared
    // settings.json — which is keyed off `dirs::config_dir()` and is the
    // same file for primary + temp + named runners. Persisting tier=Local
    // for a temp runner would silently strip the primary runner's Tier 2
    // state, so the only safe override is this in-memory overlay.
    if let Ok(raw) = std::env::var("QONTINUI_RUNNER_TIER") {
        apply_tier_env_overlay(&mut settings, &raw);
    }

    // One-shot post-upgrade detector: if Tier 2 + has a runner_token + the
    // access_token slot does NOT look like a JWT (likely the legacy opaque
    // `qontinui_runner_<random>` bearer from pre-unified-devices days), log
    // so the device_jwt_refresher knows to attempt a pair on its next tick.
    // The refresher's tick interval is 5min; user-visible recovery is
    // surfaced via WebIntegrationAuthBanner if the pair-cli call keeps
    // failing for >5min (Phase 4.3 of the unified-devices migration plan).
    if settings.tier == RunnerTier::QontinuiAccount
        && !settings.web_integration.runner_token.trim().is_empty()
    {
        let access_token = crate::auth::AuthManager::new()
            .get_access_token()
            .unwrap_or_default();
        if !crate::auth::looks_like_jwt(&access_token) {
            tracing::info!(
                "Tier 2 install detected without device-JWT (access_token slot \
                 is {} bytes, not JWT-shaped) — refresher will pair-cli on next tick",
                access_token.len()
            );
        }
    }

    settings
}

/// Parse a `QONTINUI_RUNNER_TIER` env-var value and apply it as an in-memory
/// overlay on `settings.tier`. Applied AFTER `migrate_tier_in_place` so the
/// env-var wins even when a non-empty `runner_token` was inferred to Tier 2
/// in this process. An unrecognized value is logged and ignored — the
/// (possibly migrated) persisted tier stands.
///
/// Factored out of `load_settings` so unit tests can exercise the parsing
/// without mutating the process env (see `feedback_env_var_tests_serialize`).
pub(crate) fn apply_tier_env_overlay(settings: &mut Settings, raw: &str) {
    match raw.trim().to_ascii_lowercase().as_str() {
        "local" => settings.tier = RunnerTier::Local,
        "local_provider" => settings.tier = RunnerTier::LocalProvider,
        "qontinui_account" => settings.tier = RunnerTier::QontinuiAccount,
        other => {
            error!(
                "QONTINUI_RUNNER_TIER={:?} not recognized; expected local|local_provider|qontinui_account — keeping persisted tier {:?}",
                other, settings.tier
            );
        }
    }
}

/// One-shot tier inference. When `tier_initialized` is false (i.e. the
/// loaded settings.json was written before tier existed, or the field was
/// stripped), infer the tier from `web_integration.runner_token` — a
/// non-empty token implies the user previously signed into Qontinui and
/// should land in Tier 2. Returns `true` if a migration was performed
/// (caller should persist).
///
/// Factored out so unit tests can drive it against an in-memory `Settings`
/// without touching the real settings file.
pub(crate) fn migrate_tier_in_place(settings: &mut Settings) -> bool {
    if settings.tier_initialized {
        return false;
    }
    if !settings.web_integration.runner_token.trim().is_empty() {
        settings.tier = RunnerTier::QontinuiAccount;
    } else {
        settings.tier = RunnerTier::Local;
    }
    settings.tier_initialized = true;
    true
}

/// Decide whether a pending tier/local_user_id migration may be persisted to
/// the SHARED settings.json.
///
/// Returns `true` only when there is something to persist (`needs_persist`)
/// AND the runner is the primary (`!is_secondary`). A secondary (temp or
/// named — any supervisor-launched runner with `QONTINUI_INSTANCE_NAME`) must
/// never write the shared file, because `migrate_tier_in_place` infers
/// `tier=Local` for it (no `runner_token`), which would silently demote the
/// primary's persisted Tier 2 state on disk. See the FOOTGUN GUARD comment in
/// `load_settings`.
///
/// Pure helper (no env / no IO) so the guard can be unit-tested without
/// mutating process env or touching the real settings file.
pub(crate) fn should_persist_migration(needs_persist: bool, is_secondary: bool) -> bool {
    needs_persist && !is_secondary
}

/// Save settings to file (atomic write to prevent corruption on crash)
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = get_settings_path()?;
    let contents = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    crate::fs_atomic::atomic_write(&path, contents.as_bytes())
        .map_err(|e| format!("Failed to write settings: {}", e))?;

    Ok(())
}

pub fn get_container_settings() -> crate::container::container_config::ContainerConfig {
    let settings = load_settings();
    settings.container
}

pub fn save_container_settings(
    config: crate::container::container_config::ContainerConfig,
) -> Result<(), String> {
    let mut settings = load_settings();
    settings.container = config;
    save_settings(&settings)
}

pub fn get_security_settings() -> crate::security::engine::SecuritySettings {
    let settings = load_settings();
    settings.security
}

pub fn save_security_settings(
    config: crate::security::engine::SecuritySettings,
) -> Result<(), String> {
    let mut settings = load_settings();
    settings.security = config;
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

/// Get the user-configured custom UI Bridge discovery ports
pub fn get_discovery_ports() -> Vec<u16> {
    crate::config_facade::get_discovery_ports()
}

/// Save the user-configured custom UI Bridge discovery ports
pub fn save_discovery_ports(ports: Vec<u16>) -> Result<(), String> {
    crate::config_facade::save_discovery_ports(ports)
}

/// Get the configured Claude Code config directories
pub fn get_claude_config_dirs() -> Vec<String> {
    crate::config_facade::get_claude_config_dirs()
}

/// Save the configured Claude Code config directories
pub fn save_claude_config_dirs(dirs: Vec<String>) -> Result<(), String> {
    crate::config_facade::save_claude_config_dirs(dirs)
}

/// Get custom launch commands per account config directory
pub fn get_claude_account_launch_commands() -> std::collections::HashMap<String, String> {
    crate::config_facade::get_claude_account_launch_commands()
}

/// Save custom launch commands per account config directory
pub fn save_claude_account_launch_commands(
    commands: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    crate::config_facade::save_claude_account_launch_commands(commands)
}

/// Get the current AI settings
pub fn get_ai_settings() -> AiSettings {
    crate::config_facade::get_setting::<AiSettings>()
}

/// Save AI settings
pub fn save_ai_settings(ai_settings: AiSettings) -> Result<(), String> {
    crate::config_facade::save_setting(ai_settings)
}

/// Get the World State Verifier settings.
pub fn get_world_state_verifier_settings() -> WorldStateVerifierSettings {
    crate::config_facade::get_setting::<WorldStateVerifierSettings>()
}

/// Save World State Verifier settings.
pub fn save_world_state_verifier_settings(wsv: WorldStateVerifierSettings) -> Result<(), String> {
    crate::config_facade::save_setting(wsv)
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
// Tunnel Settings (Plan 1B)
// ============================================================================

/// Get the current Tunnel settings
pub fn get_tunnel_settings() -> TunnelSettings {
    crate::config_facade::get_setting::<TunnelSettings>()
}

/// Save Tunnel settings
pub fn save_tunnel_settings(tunnel: TunnelSettings) -> Result<(), String> {
    crate::config_facade::save_setting(tunnel)
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

/// Replace the entire managed-process list atomically.
pub fn replace_managed_process_configs(
    configs: Vec<crate::process_capture::ProcessConfig>,
) -> Result<(), String> {
    crate::config_facade::replace_managed_process_configs(configs)
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
// Temp Spawn Placement Helpers
// ============================================================================

/// Get the list of temp-runner spawn placements (used by the supervisor when
/// spawning temp runners via `POST /runners/spawn-test`).
pub fn get_temp_spawn_placements() -> Vec<SpawnPlacement> {
    crate::config_facade::get_temp_spawn_placements()
}

/// Replace the temp-runner spawn placement list with the supplied list.
pub fn save_temp_spawn_placements(placements: Vec<SpawnPlacement>) -> Result<(), String> {
    crate::config_facade::save_temp_spawn_placements(placements)
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

// ============================================================================
// Saved Projects accessors
// ============================================================================

/// Get the user-curated saved project list.
/// Returns an empty `Vec` on first-run (no entry yet in `settings.json`).
pub fn get_saved_projects() -> Vec<SavedProject> {
    load_settings().saved_projects
}

/// Replace the saved-projects list atomically. Used by the wizard when the
/// user commits their project selection, and by the `saved_projects`
/// Tauri-command module.
pub fn save_saved_projects(projects: Vec<SavedProject>) -> Result<(), String> {
    let mut settings = load_settings();
    settings.saved_projects = projects;
    save_settings(&settings)
}

// ============================================================================
// Lock Yield Policy accessors
// ============================================================================

/// Get the current lock-yield policy settings.
pub fn get_lock_yield_policy_settings() -> LockYieldPolicySettings {
    load_settings().lock_yield_policy
}

/// Save the lock-yield policy settings.
pub fn save_lock_yield_policy_settings(policy: LockYieldPolicySettings) -> Result<(), String> {
    let mut settings = load_settings();
    settings.lock_yield_policy = policy;
    save_settings(&settings)
}
