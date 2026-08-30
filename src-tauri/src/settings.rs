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

impl RunnerTier {
    /// The serde snake_case `settings.json::tier` string — the same wire value
    /// `qontinui_runner_lib::profiles` reads and writes. Kept in sync with the
    /// `#[serde(rename_all = "snake_case")]` above by the round-trip test
    /// `runner_tier_as_str_matches_serde`.
    pub fn as_str(self) -> &'static str {
        match self {
            RunnerTier::Local => qontinui_runner_lib::profiles::LOCAL_TIER,
            RunnerTier::LocalProvider => "local_provider",
            RunnerTier::QontinuiAccount => qontinui_runner_lib::profiles::QONTINUI_ACCOUNT_TIER,
        }
    }
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
    /// pi coding agent CLI (`@earendil-works/pi-coding-agent`), print mode.
    /// Multi-provider by design; the runner defaults it to DeepSeek.
    PiCli,
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

impl AccountSelectionMode {
    /// The snake_case wire spelling — byte-identical to what serde emits for
    /// this enum (`#[serde(rename_all = "snake_case")]` above), pinned by
    /// `selection_mode_str_matches_serde` in this module's tests.
    ///
    /// Exists because the per-device account feed
    /// (`commands::ai_settings`'s `usage_twin_report`) puts the mode on the
    /// wire as a bare string field of a larger body — re-deriving the spelling
    /// at that call site is exactly how a shared contract drifts.
    pub fn as_str(self) -> &'static str {
        match self {
            AccountSelectionMode::Manual => "manual",
            AccountSelectionMode::LeastUsage => "least_usage",
        }
    }
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
    /// Automatically migrate a terminal Claude session to another configured
    /// account when its account runs out of tokens: a usage-limit message in
    /// the PTY output, confirmed by a fresh usage probe, triggers a
    /// transcript copy + `claude --resume` respawn under the account with the
    /// most weekly-usage headroom (see `terminal::account_migration`).
    /// No-op when fewer than two accounts are configured.
    #[serde(default = "default_auto_migrate_on_token_exhaustion")]
    pub auto_migrate_on_token_exhaustion: bool,
    /// After an automatic account migration respawns `claude --resume`, type a
    /// short continuation prompt into the resumed session (once the CLI paints
    /// its idle prompt) so autonomous work picks up where it left off instead
    /// of sitting idle at the input box. Manual migrations honor it too.
    #[serde(default = "default_auto_continue_after_migration")]
    pub auto_continue_after_migration: bool,
}

fn default_auto_migrate_on_token_exhaustion() -> bool {
    true
}

fn default_auto_continue_after_migration() -> bool {
    true
}

impl Default for ClaudeCliSettings {
    fn default() -> Self {
        Self {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            timeout_seconds: 600,
            config_dir: None,
            account_selection_mode: AccountSelectionMode::LeastUsage,
            auto_migrate_on_token_exhaustion: default_auto_migrate_on_token_exhaustion(),
            auto_continue_after_migration: default_auto_continue_after_migration(),
        }
    }
}

/// Per-rule exponential-backoff schedule for the fleet auto-response feature.
///
/// When a terminal Claude session's PTY output matches an auto-response rule's
/// regex, the runner submits the rule's prompt back into that same live session
/// after a delay that grows exponentially per consecutive match
/// (`initial_delay_secs * multiplier^attempts`). This throttles the recovery so
/// a session that keeps re-emitting the matched line (e.g. the transient "Server
/// is temporarily limiting requests" rate-limit message) is nudged with backoff
/// rather than hammered. By default the growth is UNBOUNDED (`max_delay_secs`
/// is `None`); set a cap to clamp the delay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BackoffConfig {
    #[serde(default = "default_backoff_initial_secs")]
    pub initial_delay_secs: u64,
    #[serde(default = "default_backoff_multiplier")]
    pub multiplier: f64,
    #[serde(default)]
    pub max_delay_secs: Option<u64>,
}
fn default_backoff_initial_secs() -> u64 {
    60
}
fn default_backoff_multiplier() -> f64 {
    2.0
}
impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial_delay_secs: default_backoff_initial_secs(),
            multiplier: default_backoff_multiplier(),
            max_delay_secs: None,
        }
    }
}

// NOTE: the old `AutoResponseRule` wire/cache struct was removed in Phase 4
// (unified-automation). The rule source moved from the qontinui-web backend to
// coord, whose projection (`FleetRule` with a tagged `action`) lives in
// `terminal::auto_response_fleet`. `BackoffConfig` (above) is still the shared
// backoff shape consumed by that projection.

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
///
/// Defaults to DeepSeek (`https://api.deepseek.com`, model `deepseek-chat`),
/// but any endpoint speaking the OpenAI `/chat/completions` wire format works
/// — e.g. "http://localhost:8080/v1" for a vLLM server, LM Studio, Together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiCompatibleSettings {
    /// Full base URL — e.g. "https://api.deepseek.com" (default) or
    /// "http://localhost:8080/v1" for a vLLM server.
    #[serde(
        default = "default_openai_compatible_base_url",
        deserialize_with = "deserialize_base_url_or_default"
    )]
    pub base_url: String,
    /// Model identifier the server expects (default: "deepseek-chat").
    #[serde(
        default = "default_openai_compatible_model",
        deserialize_with = "deserialize_model_or_default"
    )]
    pub model: String,
    #[serde(default = "default_openai_compatible_timeout")]
    pub timeout_seconds: u64,
    // Note: API key (if any) stored separately in OS keychain via
    // `ai_keychain().store("openai_compatible", ...)`; env fallbacks
    // DEEPSEEK_API_KEY / OPENAI_COMPATIBLE_API_KEY also work. Keyless local
    // endpoints (vLLM, LM Studio) need no key at all.
}

fn default_openai_compatible_base_url() -> String {
    "https://api.deepseek.com".to_string()
}

fn default_openai_compatible_model() -> String {
    "deepseek-chat".to_string()
}

/// Treat an absent, null, OR EMPTY `base_url` as unset and fall back to the
/// default.
///
/// `#[serde(default = ...)]` alone is not enough: it fires only when the key
/// is MISSING. Every runner install that saved AI settings while this field
/// had no meaningful default persisted `"base_url": ""` — which deserializes
/// as an empty string and silently keeps the default from ever applying. The
/// provider then fails with "base_url is empty" on a machine the operator
/// never configured by hand. Normalizing here (rather than at one call site)
/// means every reader — the API client, the connection test, the UI — sees
/// the same effective value, with no settings-file migration.
fn deserialize_base_url_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(non_empty_or(
        Option::<String>::deserialize(deserializer)?,
        default_openai_compatible_base_url,
    ))
}

/// Same empty-is-unset normalization as [`deserialize_base_url_or_default`],
/// for `model`.
fn deserialize_model_or_default<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(non_empty_or(
        Option::<String>::deserialize(deserializer)?,
        default_openai_compatible_model,
    ))
}

fn non_empty_or(raw: Option<String>, default: fn() -> String) -> String {
    match raw {
        Some(s) if !s.trim().is_empty() => s,
        _ => default(),
    }
}

fn default_openai_compatible_timeout() -> u64 {
    600
}

impl Default for OpenAiCompatibleSettings {
    fn default() -> Self {
        Self {
            base_url: default_openai_compatible_base_url(),
            model: default_openai_compatible_model(),
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

/// Settings for pi CLI execution (`@earendil-works/pi-coding-agent`).
///
/// pi is an agent CLI like Claude Code / Gemini CLI; the runner drives it in
/// print mode (`pi -p --no-session <prompt>`). `provider`/`model` are always
/// passed explicitly when set — pi silently ignores an unknown `--provider`
/// (falls back to the operator's `~/.pi/agent/settings.json` default), so
/// leaving them implicit would make provider selection machine-dependent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PiCliSettings {
    #[serde(default)]
    pub execution_mode: CliExecutionMode,
    /// Custom path to the pi executable (default: `pi` on PATH — the npm
    /// `pi.cmd` shim on Windows, driven via `cmd /c`).
    #[serde(default)]
    pub custom_path: Option<String>,
    /// pi provider name (e.g. "deepseek"). Default "deepseek".
    #[serde(default = "default_pi_cli_provider")]
    pub provider: Option<String>,
    /// pi model pattern (e.g. "deepseek-v4-flash"). `None` uses pi's default
    /// model for the selected provider.
    #[serde(default)]
    pub model: Option<String>,
    /// Comma-separated tool allowlist forwarded as `--tools` (e.g.
    /// "read,grep,find,ls" for a read-only run). `None` uses pi's defaults
    /// (`read, bash, edit, write`).
    #[serde(default)]
    pub tools: Option<String>,
    #[serde(default = "default_pi_cli_timeout")]
    pub timeout_seconds: u64,
}

fn default_pi_cli_provider() -> Option<String> {
    Some("deepseek".to_string())
}

fn default_pi_cli_timeout() -> u64 {
    600
}

impl Default for PiCliSettings {
    fn default() -> Self {
        Self {
            execution_mode: CliExecutionMode::Auto,
            custom_path: None,
            provider: default_pi_cli_provider(),
            model: None,
            tools: None,
            timeout_seconds: default_pi_cli_timeout(),
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
    /// pi CLI settings (`@earendil-works/pi-coding-agent` agent CLI).
    #[serde(default)]
    pub pi_cli: PiCliSettings,
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
    /// Federate git ops (branch/commit/push/rebase) to coord's
    /// `/coord/git-ops/record` feed. Default ON.
    ///
    /// Scoped to the `git_op` category ONLY, and read at DISPATCH time in
    /// `GitOpBridge::start_watching` — never at the shared
    /// `build_federation_ctx` layer, and never once at startup.
    ///
    /// Both halves of that are deliberate. Gating the shared context layer is
    /// what left this category switchless when the memory-named flag was
    /// deleted: `memory_federation_enabled` was named for one capability but
    /// wired at a layer common to every observable bridge, so removing the
    /// memory bridge silently removed git-op's off switch too. And reading it
    /// once at startup would make the flag unusable in practice — this fleet
    /// never restarts runners, so a startup-read control cannot be exercised
    /// at all. Per-dispatch is cheap here by the codebase's own standard:
    /// `load_settings()` already runs on hot paths (see `record_settings_fault`).
    ///
    /// Plan: `2026-07-28-git-op-federation-lost-its-kill-switch`.
    #[serde(default = "default_git_op_federation_enabled")]
    pub git_op_federation_enabled: bool,
}

fn default_interactive_sessions_enabled() -> bool {
    true
}

fn default_git_op_federation_enabled() -> bool {
    true
}

fn default_ai_path_prediction_enabled() -> bool {
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
            pi_cli: PiCliSettings::default(),
            ollama: OllamaSettings::default(),
            openai_compatible: OpenAiCompatibleSettings::default(),
            auto_refine_video_after_iterations: default_auto_refine_video_after_iterations(),
            compression: CompressionConfig::default(),
            retry: RetryConfig::default(),
            routing: RoutingConfig::default(),
            interactive_sessions_enabled: default_interactive_sessions_enabled(),
            ai_path_prediction_enabled: default_ai_path_prediction_enabled(),
            git_op_federation_enabled: default_git_op_federation_enabled(),
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

    /// Directory holding the **active** markdown plans this runner mirrors into
    /// coord work-units, and that agent sessions are pointed at via
    /// `QONTINUI_PLANS_DIR`.
    ///
    /// Default (when None): **unset — the markdown-plan tier is OFF**. There is
    /// no fallback path: a runner without this configured never scans for
    /// plans, never pushes work-units, and launches sessions without
    /// `QONTINUI_PLANS_DIR`. The coordination tiers below it (claims/intent,
    /// coord-native work-units) are unaffected.
    ///
    /// The `QONTINUI_PLAN_ADAPTER_DIR` environment variable overrides this
    /// setting when set (per-machine escape hatch).
    ///
    /// Override example: `D:\qontinui-root\plans`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plans_dir: Option<String>,

    /// An additional location plans may be read from or archived into, per the
    /// user's own convention — exported to agent sessions as
    /// `QONTINUI_PLANS_ARCHIVE_DIR`. The runner only carries the value; which
    /// of those two roles it plays is the consuming skill's business.
    ///
    /// Default (when None): **unset — sessions are told nothing about an
    /// archive**. Archiving is a file *location*, never a lifecycle status.
    /// The value is not derivable from `plans_dir` (the archive commonly lives
    /// in a different repo), so there is deliberately no fallback.
    ///
    /// Override example: `D:\qontinui-root\qontinui-dev-notes\plans`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plans_archive_dir: Option<String>,

    /// Directory holding the operator's markdown **prompts** — the third scan
    /// root of the plan & prompt library, and the value exported to agent
    /// sessions as `QONTINUI_PROMPTS_DIR`.
    ///
    /// Default (when None): **unset — sessions are told nothing about a prompts
    /// directory and the prompt scan does not run.** Deliberately *not*
    /// derivable from `plans_dir`: `/create-plan` currently guesses
    /// `$QONTINUI_PLANS_DIR/../prompts/*.md`, and that guess is what this
    /// setting exists to replace — prompts live in more than one repo and the
    /// sibling-of-plans relationship does not hold in general.
    ///
    /// Unlike `plans_dir` there is **no environment override**: that variable
    /// exists only for backward compatibility with a pre-settings deployment,
    /// and a new field has none to keep.
    ///
    /// Override example: `D:\qontinui-root\prompts`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts_dir: Option<String>,

    /// The **workspace root**: the directory holding the Qontinui repo
    /// checkouts (`<root>/qontinui-runner`, `<root>/qontinui-coord`, …). The
    /// census, the fleet publisher, the `.mcp.json` reconcile and canonical
    /// checkout creation all resolve against it.
    ///
    /// Default (when None): resolved at runtime by
    /// [`crate::workspace_paths::runner_workspace_root`] —
    /// `$QONTINUI_ROOT` → `$QONTINUI_WORKSPACE_ROOT` → **this setting** → an
    /// ancestor walk up from the running executable → `$HOME/qontinui-root`.
    /// The two environment variables outrank this setting, matching the
    /// precedence `plans_dir` already uses (a per-machine env override beats
    /// the persisted setting).
    ///
    /// **Why this setting exists.** Qontinui is open source, so the product
    /// binary must not carry the author's machine layout — the runner used to
    /// hardcode `D:/qontinui-root` as a Windows fallback in four places. That
    /// literal was the LIVE resolution path on the author's own machine, so
    /// deleting it without a bridge would have taken the census, the fleet
    /// publisher and worktree creation down together. This setting is that
    /// bridge: [`crate::workspace_paths::persist_resolved_workspace_root`]
    /// records, once, whatever the machine already resolves to, so removing
    /// the literal changes nothing for an existing install. See
    /// `2026-08-04-remove-hardcoded-machine-paths-from-product-code.md`.
    ///
    /// **Separator note.** A migration-written value on Windows carries native
    /// separators (`D:\qontinui-root`), because it comes from the executable's
    /// own path — where the deleted literal was forward-slashed
    /// (`D:/qontinui-root`). Path *joins* are unaffected, but anything that
    /// string-compares or reports this root should normalize first rather than
    /// assume either form.
    ///
    /// Override example: `D:\qontinui-root`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,

    /// When true, enforce workspace-scoped working directory resolution globally.
    /// Steps cannot resolve paths outside the workspace root.
    /// Default: false (permissive). Individual workflows can override via `strict_cwd`.
    #[serde(default)]
    pub strict_mode: bool,
}

#[cfg(test)]
mod git_op_federation_flag_tests {
    use super::*;

    /// The `git_op` kill switch defaults ON, and — critically — an
    /// AiSettings blob that predates the field must still load, taking that
    /// default rather than failing the whole settings read.
    ///
    /// This test is the durable guard against the failure that produced this
    /// plan. `memory_federation_enabled` was deleted and nothing noticed,
    /// because nothing asserted the switch existed or worked. A UI toggle was
    /// considered as the fix for that class and rejected as the weaker one: a
    /// toggle can be removed as silently as the setting. A test cannot.
    ///
    /// Plan: `2026-07-28-git-op-federation-lost-its-kill-switch`.
    #[test]
    fn git_op_federation_defaults_on_and_old_configs_still_load() {
        assert!(
            AiSettings::default().git_op_federation_enabled,
            "git_op federation must default ON — a default-off kill switch \
             would silently disable federation on every existing install"
        );

        // A pre-field config: no `git_op_federation_enabled` key at all, and
        // carrying the long-deleted `memory_federation_enabled` key that real
        // settings.json files on this fleet still hold. Both must be tolerated
        // — the unknown key because AiSettings has no `deny_unknown_fields`,
        // the missing one because of `#[serde(default = ...)]`.
        let legacy = serde_json::json!({
            "provider": AiSettings::default().provider,
            "claude_cli": AiSettings::default().claude_cli,
            "claude_api": AiSettings::default().claude_api,
            "memory_federation_enabled": false,
        });
        let parsed: AiSettings =
            serde_json::from_value(legacy).expect("a pre-field AiSettings blob must still load");
        assert!(
            parsed.git_op_federation_enabled,
            "a config written before this field existed must take the ON default"
        );

        // And an explicit opt-out must actually round-trip, or the switch is
        // decorative.
        let disabled = serde_json::json!({
            "provider": AiSettings::default().provider,
            "claude_cli": AiSettings::default().claude_cli,
            "claude_api": AiSettings::default().claude_api,
            "git_op_federation_enabled": false,
        });
        let parsed: AiSettings =
            serde_json::from_value(disabled).expect("an explicit opt-out must parse");
        assert!(
            !parsed.git_op_federation_enabled,
            "an explicit `false` must survive deserialization"
        );
    }
}

#[cfg(test)]
mod path_settings_tests {
    use super::*;

    /// Both plan directories default to unset (markdown-plan tier off) and
    /// must not appear in the serialized form when unset — an emitted `null`
    /// or `""` would be indistinguishable from "configured to nothing" for the
    /// session-env injection, which keys off absence.
    #[test]
    fn plan_dirs_default_unset_and_do_not_serialize() {
        let defaults = PathSettings::default();
        assert_eq!(defaults.plans_dir, None);
        assert_eq!(defaults.plans_archive_dir, None);
        assert_eq!(defaults.prompts_dir, None);

        let json = serde_json::to_value(&defaults).expect("PathSettings must serialize");
        let obj = json.as_object().expect("serializes as an object");
        assert!(
            !obj.contains_key("plans_dir"),
            "unset plans_dir must be absent, got {json}"
        );
        assert!(
            !obj.contains_key("plans_archive_dir"),
            "unset plans_archive_dir must be absent, got {json}"
        );
        assert!(
            !obj.contains_key("prompts_dir"),
            "unset prompts_dir must be absent, got {json}"
        );
    }

    /// Round-trip with both fields set: they serialize and come back verbatim.
    #[test]
    fn plan_dirs_round_trip_when_set() {
        let settings = PathSettings {
            dev_logs_dir: Some("/w/.dev-logs".to_string()),
            plans_dir: Some("/w/plans".to_string()),
            plans_archive_dir: Some("/w/dev-notes/plans".to_string()),
            prompts_dir: Some("/w/prompts".to_string()),
            workspace_root: Some("/w".to_string()),
            strict_mode: false,
        };

        let json = serde_json::to_string(&settings).expect("must serialize");
        let parsed: PathSettings = serde_json::from_str(&json).expect("must deserialize");

        assert_eq!(parsed.plans_dir.as_deref(), Some("/w/plans"));
        assert_eq!(
            parsed.plans_archive_dir.as_deref(),
            Some("/w/dev-notes/plans")
        );
        assert_eq!(parsed.prompts_dir.as_deref(), Some("/w/prompts"));
        assert_eq!(parsed.dev_logs_dir.as_deref(), Some("/w/.dev-logs"));
        assert_eq!(parsed.workspace_root.as_deref(), Some("/w"));
    }

    /// Settings persisted before the fields existed must still load — the
    /// `#[serde(default)]` path, i.e. every runner upgrading into this change.
    #[test]
    fn legacy_json_without_plan_dirs_deserializes() {
        let parsed: PathSettings =
            serde_json::from_str(r#"{"dev_logs_dir":"/w/.dev-logs","strict_mode":true}"#)
                .expect("legacy PathSettings JSON must still deserialize");
        assert_eq!(parsed.plans_dir, None);
        assert_eq!(parsed.plans_archive_dir, None);
        assert_eq!(parsed.prompts_dir, None);
        assert!(parsed.strict_mode);
    }

    /// `workspace_root` defaults to unset and must not serialize when unset:
    /// the resolver distinguishes "absent → fall through to the next rung" from
    /// "configured to nothing", and an emitted `null`/`""` would collapse them.
    /// A settings file written before this field existed must still load.
    #[test]
    fn workspace_root_defaults_unset_and_is_absent_from_the_serialized_form() {
        let defaults = PathSettings::default();
        assert_eq!(defaults.workspace_root, None);

        let json = serde_json::to_value(&defaults).expect("PathSettings must serialize");
        assert!(
            !json
                .as_object()
                .expect("serializes as an object")
                .contains_key("workspace_root"),
            "unset workspace_root must be absent, got {json}"
        );

        let legacy: PathSettings =
            serde_json::from_str(r#"{"dev_logs_dir":"/w/.dev-logs","strict_mode":false}"#)
                .expect("legacy PathSettings JSON must still deserialize");
        assert_eq!(legacy.workspace_root, None);
    }
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
/// `backend_url` points at the dev backend in debug builds (`http://127.0.0.1:8000`)
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
        // IPv4-pinned to match `api_config::get_api_base_url`'s debug default.
        // The backend binds IPv4 only, and `localhost` can resolve to IPv6
        // `::1` first — since `get_api_base_url` now folds this persisted value
        // in as its step-3 fallback (plan 2026-07-08), any divergence here would
        // flip an un-signed-in debug box off the IPv4 pin. Keeping them
        // byte-identical makes step 3 == step 4 for a fresh install.
        format!(
            "http://127.0.0.1:{}",
            crate::api_config::DEFAULT_BACKEND_PORT
        )
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
            // IPv4-pinned to match api_config's debug default (plan 2026-07-08).
            assert_eq!(s.backend_url, "http://127.0.0.1:8000");
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
mod cloud_sync_tests {
    use super::*;

    /// Consent contract (plan 2026-07-09-runner-session-history-cloud-sync
    /// gate 1): a fresh install AND an upgrading settings.json missing the
    /// key must both land on `cloud_sync_enabled = false` — nothing leaves
    /// the machine without an explicit opt-in.
    #[test]
    fn cloud_sync_defaults_off() {
        assert!(!Settings::default().cloud_sync_enabled);
        let parsed: Settings = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(!parsed.cloud_sync_enabled);
    }

    /// An explicit opt-in round-trips through serialization.
    #[test]
    fn cloud_sync_explicit_value_round_trips() {
        let parsed: Settings =
            serde_json::from_str(r#"{"cloud_sync_enabled": true}"#).expect("must deserialize");
        assert!(parsed.cloud_sync_enabled);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains("\"cloud_sync_enabled\":true"));
    }
}

#[cfg(test)]
mod memory_link_expansion_tests {
    use super::*;

    /// Plan `2026-07-29-memory-link-expansion-retrieval-arm` Phase 3: the
    /// link-expansion retrieval arm ships default-OFF until the recall-efficacy
    /// harness can measure it. Both a fresh install (`Settings::default()`) and
    /// an upgrading settings.json missing the key must land on `false`.
    #[test]
    fn memory_link_expansion_defaults_off() {
        assert!(!Settings::default().memory_link_expansion_enabled);
        let parsed: Settings = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(!parsed.memory_link_expansion_enabled);
    }

    /// An explicit opt-in round-trips through serialization, so the flag can be
    /// flipped on once the harness lands.
    #[test]
    fn memory_link_expansion_explicit_value_round_trips() {
        let parsed: Settings = serde_json::from_str(r#"{"memory_link_expansion_enabled": true}"#)
            .expect("must deserialize");
        assert!(parsed.memory_link_expansion_enabled);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains("\"memory_link_expansion_enabled\":true"));
    }
}

#[cfg(test)]
mod session_metadata_sync_tests {
    use super::*;

    /// Consent contract (plan `2026-07-10-split-cloud-sync-consent` gate 2):
    /// this half carries no conversation content, so it defaults ON both for
    /// a genuinely fresh install (`Settings::default()`) and for an empty
    /// settings.json (`{}`).
    #[test]
    fn session_metadata_sync_defaults_true_on_fresh_settings() {
        assert!(Settings::default().session_metadata_sync_enabled);
        let parsed: Settings = serde_json::from_str("{}").expect("empty object must deserialize");
        assert!(parsed.session_metadata_sync_enabled);
    }

    /// An explicit value (either direction) round-trips through
    /// serialization untouched.
    #[test]
    fn session_metadata_sync_explicit_value_round_trips() {
        let parsed: Settings = serde_json::from_str(r#"{"session_metadata_sync_enabled": false}"#)
            .expect("must deserialize");
        assert!(!parsed.session_metadata_sync_enabled);
        let json = serde_json::to_string(&parsed).unwrap();
        assert!(json.contains("\"session_metadata_sync_enabled\":false"));
    }

    /// Migration: a pre-split settings.json with `cloud_sync_enabled: true`
    /// and no new key carries the legacy opt-in forward.
    #[test]
    fn migrate_legacy_true_carries_forward() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"cloud_sync_enabled": true}"#).unwrap();
        // Start at the opposite value so the assertion below proves the fn
        // actively wrote it, not that it was already true.
        let mut settings: Settings =
            serde_json::from_str(r#"{"session_metadata_sync_enabled": false}"#).unwrap();
        migrate_metadata_sync_flag(&raw, &mut settings);
        assert!(settings.session_metadata_sync_enabled);
    }

    /// Migration: a pre-split settings.json with `cloud_sync_enabled: false`
    /// and no new key carries the legacy opt-out forward — this is the
    /// consent-preserving case (must NOT silently flip to the new true
    /// default).
    #[test]
    fn migrate_legacy_false_carries_forward() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"cloud_sync_enabled": false}"#).unwrap();
        let mut settings = Settings::default();
        assert!(settings.session_metadata_sync_enabled); // starts true
        migrate_metadata_sync_flag(&raw, &mut settings);
        assert!(!settings.session_metadata_sync_enabled);
    }

    /// Migration: once the new key is present, it is authoritative — a
    /// legacy `cloud_sync_enabled: false` alongside an explicit
    /// `session_metadata_sync_enabled: true` must NOT be overwritten by the
    /// legacy value.
    #[test]
    fn migrate_new_key_present_ignores_legacy() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"{"cloud_sync_enabled": false, "session_metadata_sync_enabled": true}"#,
        )
        .unwrap();
        let mut settings: Settings =
            serde_json::from_str(r#"{"session_metadata_sync_enabled": true}"#).unwrap();
        migrate_metadata_sync_flag(&raw, &mut settings);
        assert!(settings.session_metadata_sync_enabled);
    }

    /// Migration: neither key present (genuinely fresh JSON content) leaves
    /// the already-defaulted value untouched.
    #[test]
    fn migrate_neither_key_present_leaves_default_untouched() {
        let raw: serde_json::Value = serde_json::from_str("{}").unwrap();
        let mut settings = Settings::default();
        assert!(settings.session_metadata_sync_enabled);
        migrate_metadata_sync_flag(&raw, &mut settings);
        assert!(settings.session_metadata_sync_enabled);
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

        let migrated = migrate_tier_in_place(
            &mut s, /* server_mode = */ false, /* paired = */ false,
            /* disk_runner_token = */ true,
        );
        assert!(migrated.changed(), "must report migration performed");
        assert!(
            migrated.persists(),
            "a runner_token is a fact on disk, so its promotion is durable"
        );
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

        let migrated = migrate_tier_in_place(
            &mut s, /* server_mode = */ false, /* paired = */ false,
            /* disk_runner_token = */ false,
        );
        assert!(migrated.changed());
        assert_eq!(s.tier, RunnerTier::Local);
        assert!(s.tier_initialized);
    }

    /// Tier-inference must be a one-shot: once `tier_initialized` is true,
    /// subsequent loads must not overwrite a deliberate user tier choice.
    /// Tier 1 is closed to inference: nothing but `set_runner_tier` can
    /// produce `local_provider`, so finding it on disk IS an explicit choice —
    /// which is why this stays a no-op even though the Phase 3 unlatch removed
    /// the blanket `tier_initialized` early return.
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

        let migrated = migrate_tier_in_place(
            &mut s, /* server_mode = */ false, /* paired = */ false,
            /* disk_runner_token = */ true,
        );
        assert!(!migrated.changed(), "must not re-migrate when initialized");
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

    /// [`RunnerTier::as_str`] is the bridge between the bin's typed tier and
    /// the lib's string one (`profiles::infer_tier` /
    /// `tier_is_open_to_inference` speak strings, because the lib has no
    /// `Settings`). A drift between it and the serde representation would let
    /// the two tier readers disagree about the same document — exactly the
    /// class of defect the shared inference removed — so it is pinned against
    /// serde itself rather than against a second hand-written literal.
    #[test]
    fn runner_tier_as_str_matches_serde() {
        for t in [
            RunnerTier::Local,
            RunnerTier::LocalProvider,
            RunnerTier::QontinuiAccount,
        ] {
            let via_serde = serde_json::to_value(t).unwrap();
            assert_eq!(
                via_serde,
                serde_json::Value::String(t.as_str().to_string()),
                "as_str drifted from the serde wire value for {t:?}"
            );
        }
    }

    /// The upgrade path, at the struct level: a settings document written
    /// before Phase 3 has no `tier_chosen_explicitly` key and must read
    /// `false` — "never chosen", hence eligible for re-inference. The
    /// behavioural half is `tier_matrix_tests`'
    /// `settings_without_tier_chosen_explicitly_reads_false`.
    #[test]
    fn tier_chosen_explicitly_defaults_false_on_a_pre_phase_3_document() {
        let s: Settings =
            serde_json::from_str(r#"{"tier":"local","tier_initialized":true}"#).unwrap();
        assert!(!s.tier_chosen_explicitly);
        assert!(
            !Settings::default().tier_chosen_explicitly,
            "and a fresh install has made no choice either"
        );
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
        let ok = SettingsProvenance::Loaded;
        // Secondary with a pending migration: persist is suppressed.
        assert!(
            !should_persist_migration(true, /* is_secondary = */ true, ok),
            "a secondary runner must never persist a migration to the shared settings.json"
        );
        // Primary with a pending migration: persist proceeds.
        assert!(
            should_persist_migration(true, /* is_secondary = */ false, ok),
            "the primary runner must persist its tier/local_user_id migration"
        );
        // Nothing to persist: never persist, regardless of runner kind.
        assert!(!should_persist_migration(false, false, ok));
        assert!(!should_persist_migration(false, true, ok));
    }

    // ------------------------------------------------------------------
    // C1/C2/C3 — settings provenance
    // ------------------------------------------------------------------

    /// THE headline invariant: a `Settings` that was defaulted only because
    /// the real file was unreadable must NEVER be written back to disk. Doing
    /// so atomically replaced a corrupt-but-recoverable settings.json with an
    /// all-defaults one (tier=Local, empty runner_token, setup_completed=false)
    /// and logged it as a successful migration.
    #[test]
    fn unreadable_settings_are_never_persisted() {
        assert!(
            !should_persist_migration(true, false, SettingsProvenance::Unreadable),
            "a defaulted-because-unreadable struct must never be persisted, \
             even by the primary runner with a pending migration"
        );
        assert!(
            !should_persist_migration(true, true, SettingsProvenance::Unreadable),
            "…and certainly not by a secondary"
        );
    }

    /// A genuine first run (no file yet) is authoritative: defaults ARE the
    /// user's state, so the tier/local_user_id migration may persist and seed
    /// the file. Regression guard against over-correcting C2 into "never
    /// persist anything", which would leave `local_user_id` unminted forever.
    #[test]
    fn fresh_install_still_persists_its_migration() {
        assert!(
            should_persist_migration(true, false, SettingsProvenance::FreshInstall),
            "a genuine first run must still seed settings.json"
        );
    }

    #[test]
    fn provenance_authoritativeness_matrix() {
        assert!(SettingsProvenance::Loaded.is_authoritative());
        assert!(SettingsProvenance::FreshInstall.is_authoritative());
        assert!(
            !SettingsProvenance::Unreadable.is_authoritative(),
            "unreadable means UNKNOWN — it must never be treated as real state"
        );
    }

    /// An unreadable load must not be reported as a definitive tier. Callers
    /// that gate capability on the tier have to be able to tell "the user is
    /// Local" from "we do not know the user's tier".
    #[test]
    fn tier_resolution_distinguishes_unknown_from_local() {
        let local = TierResolution::Known(RunnerTier::Local);
        let unknown = TierResolution::Unknown {
            reason: "boom".to_string(),
        };
        assert_ne!(local, unknown);
        assert_eq!(local.known(), Some(RunnerTier::Local));
        assert_eq!(unknown.known(), None, "unknown must not collapse to Local");
        assert_eq!(local.as_str(), "local");
        assert_eq!(unknown.as_str(), "unknown");
        assert_eq!(
            TierResolution::Known(RunnerTier::QontinuiAccount).as_str(),
            "qontinui_account"
        );
    }

    /// The unreadable message must name the file + the real remediation, and
    /// must NOT tell the user to sign in (which is the wrong CTA and the
    /// misleading behavior C4 called out).
    #[test]
    fn unreadable_message_does_not_suggest_signing_in() {
        let loaded = LoadedSettings {
            settings: Settings::default(),
            provenance: SettingsProvenance::Unreadable,
            error: Some("parse failed: expected value at line 1".to_string()),
        };
        let msg = loaded.unreadable_message();
        assert!(msg.contains("settings.json could not be read"), "{msg}");
        assert!(msg.contains("parse failed"), "{msg}");
        assert!(msg.contains("Nothing was changed"), "{msg}");
        assert!(
            !msg.to_lowercase().contains("sign in"),
            "must not render a sign-in CTA for a file-read fault: {msg}"
        );
    }

    /// `read_settings_from_disk` decides provenance from the file; this locks
    /// in the classification of the three shapes without touching the real
    /// user settings file (the classifier is exercised through its inputs).
    #[test]
    fn parse_failure_classifies_as_unreadable_not_fresh() {
        // A truncated/corrupt document must not deserialize — if it ever did,
        // the Unreadable branch could never be reached and C1 would regress.
        assert!(
            serde_json::from_str::<Settings>("{\"tier\": \"qontinui_acc").is_err(),
            "a truncated settings.json must fail to parse"
        );
        // …while an empty object is a legitimate (authoritative) document.
        assert!(serde_json::from_str::<Settings>("{}").is_ok());
    }
}

#[cfg(test)]
mod ci_node_tests {
    use super::*;

    /// Contract: a fresh install and an empty settings.json both land on the
    /// fully-inert CI-node default — disabled, 1 slot, EMPTY allowlist
    /// (nothing runnable), 20 GiB disk floor.
    #[test]
    fn ci_node_defaults_are_inert() {
        for s in [
            Settings::default(),
            serde_json::from_str::<Settings>("{}").expect("empty object must deserialize"),
        ] {
            assert!(!s.ci_node.enabled);
            assert_eq!(s.ci_node.max_concurrent_builds, 1);
            assert!(s.ci_node.repo_allowlist.is_empty());
            assert_eq!(s.ci_node.min_free_disk_gb, 20);
        }
    }

    /// Explicit values round-trip untouched (the setting is hand-edited
    /// JSON until a Settings UI ships — parse fidelity is the whole UX).
    #[test]
    fn ci_node_explicit_values_round_trip() {
        let parsed: Settings = serde_json::from_str(
            r#"{"ci_node": {"enabled": true, "max_concurrent_builds": 2,
                 "repo_allowlist": ["qontinui/qontinui-runner"], "min_free_disk_gb": 50}}"#,
        )
        .expect("must deserialize");
        assert!(parsed.ci_node.enabled);
        assert_eq!(parsed.ci_node.max_concurrent_builds, 2);
        assert_eq!(
            parsed.ci_node.repo_allowlist,
            vec!["qontinui/qontinui-runner".to_string()]
        );
        assert_eq!(parsed.ci_node.min_free_disk_gb, 50);
        let json = serde_json::to_string(&parsed).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ci_node, parsed.ci_node);
    }

    /// A partial ci_node object fills the missing keys from the serde
    /// defaults (enabled alone must not zero the disk floor or slot count).
    #[test]
    fn ci_node_partial_object_fills_defaults() {
        let parsed: Settings = serde_json::from_str(r#"{"ci_node": {"enabled": true}}"#).unwrap();
        assert!(parsed.ci_node.enabled);
        assert_eq!(parsed.ci_node.max_concurrent_builds, 1);
        assert!(parsed.ci_node.repo_allowlist.is_empty());
        assert_eq!(parsed.ci_node.min_free_disk_gb, 20);
    }
}

#[cfg(test)]
mod session_guard_tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// Contract: a fresh install and an empty settings.json both land on the
    /// LIVE conservative default — enabled, 3 GiB warn, 1.5 GiB critical.
    /// Unlike `ci_node`, "no key present" must NOT mean "no protection": the
    /// unguarded state is the one the fleet was in on the night of the
    /// 2026-08-06→07 incident.
    #[test]
    fn session_guard_defaults_are_live_not_inert() {
        for s in [
            Settings::default(),
            serde_json::from_str::<Settings>("{}").expect("empty object must deserialize"),
        ] {
            assert!(s.session_guard.enabled);
            assert_eq!(s.session_guard.warn_free_commit_bytes, 3 * GIB);
            assert_eq!(s.session_guard.critical_free_commit_bytes, 3 * GIB / 2);
        }
    }

    /// The ladder's ordering, pinned. `critical < warn` (the heavier verdict
    /// fires later) and `warn < ci_node`'s 4 GiB hard-reject floor — the
    /// ordering `ci_node::admission::MIN_FREE_COMMIT_GB`'s doc comment argues
    /// for. A default that inverted this would make the mildest consequence
    /// the first to fire.
    #[test]
    fn default_floors_sit_below_the_ci_node_reject_floor() {
        let g = SessionGuardSettings::default();
        assert!(
            g.critical_free_commit_bytes < g.warn_free_commit_bytes,
            "critical must be the lower floor — it is the heavier verdict"
        );
        assert!(
            g.warn_free_commit_bytes < crate::ci_node::admission::MIN_FREE_COMMIT_GB * GIB,
            "a warn is lighter than ci_node's hard reject, so it must sit below it"
        );
    }

    /// Explicit values round-trip untouched.
    #[test]
    fn session_guard_explicit_values_round_trip() {
        let parsed: Settings = serde_json::from_str(
            r#"{"session_guard": {"warn_free_commit_bytes": 8589934592,
                 "critical_free_commit_bytes": 4294967296, "enabled": false}}"#,
        )
        .expect("must deserialize");
        assert_eq!(parsed.session_guard.warn_free_commit_bytes, 8 * GIB);
        assert_eq!(parsed.session_guard.critical_free_commit_bytes, 4 * GIB);
        assert!(!parsed.session_guard.enabled);
        let json = serde_json::to_string(&parsed).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_guard, parsed.session_guard);
    }

    /// A partial object fills the missing keys from the serde defaults —
    /// tightening the warn floor alone must not silently zero (i.e. disable)
    /// the critical floor.
    #[test]
    fn session_guard_partial_object_fills_defaults() {
        let parsed: Settings =
            serde_json::from_str(r#"{"session_guard": {"warn_free_commit_bytes": 6442450944}}"#)
                .unwrap();
        assert_eq!(parsed.session_guard.warn_free_commit_bytes, 6 * GIB);
        assert_eq!(parsed.session_guard.critical_free_commit_bytes, 3 * GIB / 2);
        assert!(parsed.session_guard.enabled);
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

// ============================================================================
// Helper Task Queue Settings (plan 2026-06-29-helper-task-queue, Phase 1.3)
// ============================================================================

/// Owner controls for the Helper Task Queue emit surface.
///
/// When `emit_enabled` is true, runner subsystems (currently the yellow-band
/// spec-check hook in `spec_api::spec_check`) emit human-judgment micro-tasks
/// to coord's helper-task broker via `helper_tasks::HelperTaskRegistrar`.
/// `emit_kinds` scopes which task kinds may be emitted — Phase 1 ships only
/// `"spot_check"`. Default is OFF so no helper tasks leave the runner until
/// the owner opts in via Settings (Helper Tasks page → Config tab).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelperTasksSettings {
    /// Master emit toggle. Default FALSE — opt-in only.
    #[serde(default)]
    pub emit_enabled: bool,
    /// Task kinds the runner may emit (snake_case wire values, e.g.
    /// `"spot_check"`). Emit sites check membership before emitting.
    #[serde(default = "default_helper_task_emit_kinds")]
    pub emit_kinds: Vec<String>,
}

fn default_helper_task_emit_kinds() -> Vec<String> {
    vec!["spot_check".to_string()]
}

impl Default for HelperTasksSettings {
    fn default() -> Self {
        Self {
            emit_enabled: false,
            emit_kinds: default_helper_task_emit_kinds(),
        }
    }
}

// ============================================================================
// CI Node Settings (plan 2026-07-15-runner-as-ci-node-migration, Phase 0)
// ============================================================================

/// Opt-in "act as CI node" settings (plan
/// `2026-07-15-runner-as-ci-node-migration`, Phase 0). When `enabled`, the
/// fleet heartbeat advertises the **`ci_node`** capability (deliberately NOT
/// `ci_runner` — that string would inflate coord's merge-probe capacity while
/// the dispatch lane is still dark, plan §7.1) plus warmth/platform labels,
/// and the budget publish reports `max_concurrent_builds` instead of the
/// hardcoded 0. The `ci_node` executor module only admits dispatches for
/// repos in `repo_allowlist` — an empty allowlist means NOTHING is runnable
/// even when enabled.
///
/// Default is fully OFF; a missing key in an existing settings.json loads as
/// the inert default.
///
/// Two ways to change it: hand-edit `settings.json`, or configure the machine
/// in qontinui-web (`/environments/machines`), which reaches this struct via
/// coord's `POST /devenv/ci-node-dispatch` →
/// `events.ci.settings_requested.<device_id>` →
/// [`crate::ci_node::settings_directive`]. The remote door re-validates every
/// field locally and can only ever write values a hand-edit could also write —
/// notably it CANNOT write a wildcard allowlist or a `min_free_disk_gb` of 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CiNodeSettings {
    /// Master opt-in. Default FALSE — the runner never advertises CI
    /// capacity or accepts CI dispatches until the owner flips this.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum concurrent CI builds admitted by the executor, and the value
    /// advertised via the device budget POST when enabled. Default 1.
    #[serde(default = "default_ci_node_max_concurrent_builds")]
    pub max_concurrent_builds: u32,
    /// Repos this device may build. Entries match either the coord
    /// `owner/name` slug or the bare repo basename. Empty (the default)
    /// means no repo is runnable — allowlisting is a deliberate act.
    #[serde(default)]
    pub repo_allowlist: Vec<String>,
    /// Minimum free disk (GiB) on the QONTINUI_ROOT volume required to
    /// START a build; below this the dispatch is refused with a reason
    /// (this box has hit `os error 112` — disk exhaustion is real).
    #[serde(default = "default_ci_node_min_free_disk_gb")]
    pub min_free_disk_gb: u64,
    /// May a dispatch CONVERGE this box's global toolchains toward canonical?
    ///
    /// Default FALSE, and the default is the whole point. A manifest's
    /// `[canonical]` block declares that a build REQUIRES the box to be at
    /// canonical; it must never also be the thing that authorises rewriting
    /// the owner's rustup/volta/pyenv installation, because the manifest is a
    /// file in someone else's repository. So the requirement comes from the
    /// repo and the authority comes from here — the same split, and the same
    /// reason, as `repo_allowlist` deciding which repos' commands may run at
    /// all.
    ///
    /// With this false a drifted box does not silently build anyway: the
    /// dispatch is REFUSED with the drift named (`ci_node::canonical`). Off
    /// means "do not touch my toolchains", not "ignore the declaration".
    #[serde(default)]
    pub canonical_converge: bool,
}

fn default_ci_node_max_concurrent_builds() -> u32 {
    1
}

fn default_ci_node_min_free_disk_gb() -> u64 {
    20
}

impl Default for CiNodeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_concurrent_builds: default_ci_node_max_concurrent_builds(),
            repo_allowlist: Vec::new(),
            min_free_disk_gb: default_ci_node_min_free_disk_gb(),
            canonical_converge: false,
        }
    }
}

// ============================================================================
// Session Guard Settings (plan
// 2026-08-07-runner-resource-guard-and-session-protection, Part B item 2)
// ============================================================================

/// Machine-owner floors that protect **live interactive sessions** from being
/// killed by Windows commit exhaustion.
///
/// This answers a different question from [`CiNodeSettings`]: not "may coord
/// send this box more CI work" but "is this box safe enough to keep letting the
/// user start new sessions at all" — a question that must be answerable
/// locally, immediately, and with nothing dispatched by anyone. Overnight
/// 2026-08-06→07 Windows' Resource-Exhaustion-Detector fired 12 times naming
/// `vmmemWSL` plus concurrent `rustc.exe`, and several Claude Code sessions
/// died inside runner-spawned terminals while the terminal windows stayed open.
/// The runner never saw it because nothing local was watching.
///
/// ## Bytes, not GiB
///
/// Every floor here is in **bytes**, deliberately, even though the neighbouring
/// [`CiNodeSettings::min_free_disk_gb`] and
/// [`crate::ci_node::admission::MIN_FREE_COMMIT_GB`] are in GiB. The critical
/// floor is 1.5 GiB, which has no integer-GiB spelling, and coord's own
/// fleet-policy floors are `*_bytes` columns
/// (`min_free_mem_bytes_host` / `_wsl`, `min_free_disk_bytes`) — so bytes is
/// both the only unit that can express the value and the unit the fleet default
/// will arrive in when Part B item 3 wires the poller up. One unit across the
/// local override and the fleet default is what keeps `max(local, fleet,
/// hardcoded)` a comparison rather than a conversion.
///
/// ## Why the warn floor sits BELOW ci_node's 4 GiB
///
/// The lanes on this machine deliberately differ by **verdict**, not by
/// quantity — they all read Windows free commit
/// ([`crate::fleet::resource_sample::available_commit_bytes`], plan §A3).
/// `cargo-guard.sh` defers at 5 GiB, the supervisor's build pool defers at
/// 5 GiB, and `ci_node` hard-**rejects** at 4 GiB. The doc comment on
/// [`crate::ci_node::admission::MIN_FREE_COMMIT_GB`] (`ci_node/admission.rs`,
/// "The number, and why it is LOWER than the supervisor's 5") argues exactly
/// this ordering: a rejecting lane must sit below a deferring one or it turns
/// away work the deferring lane would have run a minute later.
///
/// A **warn** is lighter than all three verdicts, so it sits lowest of all:
/// 3 GiB. The **critical** floor is the heaviest verdict in the whole ladder —
/// it blocks a human's own spawn — which is why it gets the lowest number
/// (1.5 GiB) and an explicit override at the point of refusal. Raising the warn
/// floor above `ci_node`'s 4 GiB would invert the ladder and make the mildest
/// consequence the first to fire.
///
/// A missing `session_guard` key in an existing `settings.json` loads with
/// these defaults via the per-field `#[serde(default = …)]`s, exactly like
/// [`CiNodeSettings`] — so an upgrade gets the conservative floors without a
/// migration, and a hand-edit that names only one field keeps the defaults for
/// the rest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionGuardSettings {
    /// Free **commit** (bytes) below which spawning a new session is still
    /// allowed but the user is warned, naming the current headroom and this
    /// floor. Default 3 GiB.
    #[serde(default = "default_session_guard_warn_free_commit_bytes")]
    pub warn_free_commit_bytes: u64,
    /// Free **commit** (bytes) below which a new spawn is refused by default.
    /// The refusal is always overridable — a false positive here blocks the
    /// user's actual work, which is a worse failure than an occasional missed
    /// warning. Default 1.5 GiB.
    #[serde(default = "default_session_guard_critical_free_commit_bytes")]
    pub critical_free_commit_bytes: u64,
    /// Master switch for the whole guard. Default **TRUE**, unlike
    /// [`CiNodeSettings::enabled`]: `ci_node` opts a machine INTO accepting
    /// remote work, so off is the safe default there; this guard only ever
    /// warns the owner about their own machine, so off is the *unsafe* default
    /// — it is the state the fleet was already in on the night of the incident.
    #[serde(default = "default_session_guard_enabled")]
    pub enabled: bool,
}

/// 3 GiB — see [`SessionGuardSettings`] on why the warn floor sits below
/// `ci_node`'s 4 GiB reject floor.
fn default_session_guard_warn_free_commit_bytes() -> u64 {
    3 * 1024 * 1024 * 1024
}

/// 1.5 GiB — the heaviest verdict in the ladder gets the lowest number.
fn default_session_guard_critical_free_commit_bytes() -> u64 {
    3 * 1024 * 1024 * 1024 / 2
}

fn default_session_guard_enabled() -> bool {
    true
}

impl Default for SessionGuardSettings {
    fn default() -> Self {
        Self {
            warn_free_commit_bytes: default_session_guard_warn_free_commit_bytes(),
            critical_free_commit_bytes: default_session_guard_critical_free_commit_bytes(),
            enabled: default_session_guard_enabled(),
        }
    }
}

// `Clone` exists so [`read_settings_from_disk`] can serve a cached parse
// instead of re-reading and re-parsing the file on every call (twice — once to
// `Settings`, once to `serde_json::Value` for the migration check). Handing a
// caller a clone of an already-parsed document is strictly cheaper than the
// parse it replaces; it is not an invitation to clone settings casually.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    /// Default AI launch command template for accounts without a per-account
    /// override. `None` = built-in default. `{sessionId}` is substituted with
    /// the fresh pinned session id; without it `--session-id <uuid>` is appended.
    #[serde(default)]
    pub claude_default_launch_command: Option<String>,
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
    /// `SavedProject.id` of the currently-active project, or `None` when the
    /// user has not picked one yet.
    ///
    /// Deliberately server-side rather than in the frontend's
    /// port-namespaced `instanceStorage`: only `settings.json` lets the
    /// runner restore (and optionally auto-start) the last project at boot
    /// before any frontend has mounted, lets the MCP/HTTP API answer "which
    /// project is current" so an agent's tools inherit the root, and
    /// survives a cleared WebView2 profile.
    #[serde(default)]
    pub active_project_id: Option<String>,
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
    /// Set true **only** by `commands::auth::set_runner_tier` — the
    /// SetupWizard's TierStep, i.e. the operator's own, explicit choice.
    ///
    /// This is tier PROVENANCE, and it is what makes the inference safe to
    /// re-run. `tier_initialized` cannot answer "did a human pick this?":
    /// the inference and `set_runner_tier` both write it, so "never chosen"
    /// and "chose Local" were indistinguishable — and conflating them means
    /// silently re-promoting a box whose owner deliberately opted out.
    ///
    /// `#[serde(default)]` ⇒ every install written before this field existed
    /// reads `false` ("never chosen") and is therefore eligible for
    /// re-inference. That is deliberately the upgrade path (pinned by
    /// `settings_without_tier_chosen_explicitly_reads_false`), and it is safe
    /// because the re-inference can only promote — see
    /// `qontinui_runner_lib::profiles::tier_is_open_to_inference`.
    ///
    /// Written by nothing else: not by `migrate_tier_in_place`, not by the
    /// pair doors, not by `qontinui_sign_out`.
    #[serde(default)]
    pub tier_chosen_explicitly: bool,
    /// Per-`~/.qontinui/`-dir UUID identifying this install for local-DB rows.
    /// Populated lazily by `load_settings` when empty. Persists across Tier
    /// upgrades and Tier-2 sign-outs — never replaced by the Qontinui user id.
    #[serde(default)]
    pub local_user_id: String,
    /// Qontinui user id (from the access token's `sub` claim). Filled on
    /// Tier-2 sign-in, cleared on sign-out. `local_user_id` stays alongside.
    #[serde(default)]
    pub qontinui_user_id: Option<String>,
    /// Helper Task Queue owner controls (emit toggle + allowed kinds).
    /// Default OFF — see `HelperTasksSettings`.
    #[serde(default)]
    pub helper_tasks: HelperTasksSettings,
    /// Cloud session sync consent — gate 1 of the session-history cloud-sync
    /// consent model (plan `2026-07-09-runner-session-history-cloud-sync`
    /// §3.1). When true, AI conversation transcript chunks and terminal
    /// session records are mirrored to the operator's coord tenant via the
    /// session outbox (warm tier ~7 days post-close, cold archive 90 days).
    /// Default FALSE — with the toggle off the feature is inert: no outbox
    /// entries, no network egress, nothing leaves this machine. A missing
    /// key in an existing settings.json loads as false.
    #[serde(default)]
    pub cloud_sync_enabled: bool,
    /// Session-metadata sync consent — gate 2 of the session-history
    /// cloud-sync consent model (split from `cloud_sync_enabled` per plan
    /// `2026-07-10-split-cloud-sync-consent`). When true, restore-registry
    /// metadata (provider, cwd, launch command, restore tier, machine id —
    /// NO conversation content) is mirrored to the operator's coord tenant
    /// so a session can be resumed or handed off from another machine.
    /// Default TRUE — this half carries no conversation content, so it is
    /// opt-out rather than opt-in. A missing key in a settings.json written
    /// before this split migrates forward from the legacy
    /// `cloud_sync_enabled` value (see `migrate_metadata_sync_flag` in
    /// `load_settings`) rather than silently flipping to the new default.
    #[serde(default = "default_session_metadata_sync_enabled")]
    pub session_metadata_sync_enabled: bool,
    /// CI-node opt-in (plan `2026-07-15-runner-as-ci-node-migration`,
    /// Phase 0). Default fully off — see [`CiNodeSettings`]. Read-only at
    /// runtime (heartbeat + executor); no save path exists yet, so the
    /// multi-instance persist guard (`should_persist_migration`) is not in
    /// play for this field.
    #[serde(default)]
    pub ci_node: CiNodeSettings,
    /// Live-session protection floors (plan
    /// `2026-08-07-runner-resource-guard-and-session-protection`, Part B).
    /// Distinct from [`ci_node`](Settings::ci_node): those floors decide
    /// whether coord may send this box CI work, these decide whether the box is
    /// safe enough to start another interactive session on. Defaults are live
    /// (`enabled = true`, 3 GiB warn / 1.5 GiB critical) — see
    /// [`SessionGuardSettings`].
    #[serde(default)]
    pub session_guard: SessionGuardSettings,
    /// Ask the cloud memory endpoint (`POST /api/v1/memory/query`) for the
    /// link-expansion retrieval arm — the third RRF arm that one-hop-expands
    /// over `coord.memory_links` (plan
    /// `2026-07-29-memory-link-expansion-retrieval-arm`, Phase 4).
    ///
    /// Default FALSE, deliberately: that plan's Phase 3 ships the arm
    /// default-off and turns it on only once the efficacy harness in
    /// `2026-07-29-memory-recall-efficacy-benchmark` can measure whether it
    /// helps recall or just adds noise — and that plan is still DRAFT. Sending
    /// `link_expansion: true` unconditionally would default an unmeasured
    /// ranking change ON, which is exactly what Phase 3 forbids. The flag makes
    /// the wire-through shippable now and flippable later. A missing key in an
    /// existing settings.json loads as false.
    #[serde(default)]
    pub memory_link_expansion_enabled: bool,
    /// Performance caps and tiering knobs (plan
    /// `2026-07-28-runner-many-sessions-performance` Phase 8). Every field
    /// defaults to the value that was hardcoded before the phase, so a
    /// settings.json without the key behaves exactly as it did.
    #[serde(default)]
    pub performance: PerformanceSettings,
    /// Per-run AI cost cap (plan
    /// `2026-08-20-workflow-resume-reexecutes-and-rebills`, Phase 5).
    ///
    /// `$5.00 / 500,000 tokens` used to be a hardcoded constant reachable only
    /// through a `RunCostTrackers::with_budget` constructor that no production
    /// code called — configurable-looking, configured by nothing. That
    /// constructor has been deleted in favour of this key. Every field
    /// defaults to exactly that constant, so a `settings.json` without this
    /// key behaves identically. Read once per task run (via
    /// [`crate::cost_management::budget::TokenBudget::from_settings`]), so a
    /// change is live for the next run with no restart.
    #[serde(default)]
    pub cost_budget: crate::cost_management::budget::TokenBudget,
}

fn default_session_metadata_sync_enabled() -> bool {
    true
}

impl Default for Settings {
    /// Deliberately NOT `#[derive(Default)]`: a derived impl would build each
    /// field from that field's own `Default::default()`, which silently
    /// ignores every `#[serde(default = "fn")]` attribute above (those are
    /// serde-deserialize-only; the standard `Default` derive doesn't read
    /// them). Several bool fields here (e.g.
    /// `session_metadata_sync_enabled`, `auto_load_last_config`) intend a
    /// `true` default via their serde default fn even on a genuinely fresh
    /// install with no settings file — the branch that constructs
    /// `Settings::default()` directly in `load_settings()`. Round-tripping
    /// through an empty JSON object applies every field's serde default
    /// uniformly (the same mechanism `serde_json::from_str::<Settings>("{}")`
    /// already exercises in the tests below), so `Settings::default()` and
    /// "load a settings.json containing `{}`" agree, as they should.
    fn default() -> Self {
        serde_json::from_str("{}")
            .expect("Settings must deserialize from an empty JSON object — every field needs a serde default")
    }
}

// ============================================================================
// Performance Settings (plan 2026-07-28-runner-many-sessions-performance §8)
// ============================================================================

/// Operator-tunable caps for the many-sessions performance work.
///
/// Phase 8 of `plans/2026-07-28-runner-many-sessions-performance.md` — the
/// "safety rail". Every knob here was a hardcoded constant before this
/// struct existed, and **every default reproduces that constant exactly**,
/// so a `settings.json` written before this key existed loads and behaves
/// identically. Nothing in here can refuse work: the session knob is a
/// display threshold for an advisory banner, deliberately never a cap (plan
/// §5 rejects a hard session cap as a capability regression).
///
/// Read through [`get_performance_settings`], which serves a process-cached
/// snapshot so the terminal-spawn path never pays a settings-file read for
/// these values. [`save_performance_settings`] refreshes that cache, so a
/// save is live for the *next* terminal spawn with no restart. The two
/// grid-scan loops are the exception — they read their interval once when
/// spawned at startup, so a change there needs a runner restart (the
/// settings UI says so).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerformanceSettings {
    /// Maximum terminal panes allowed to hold a WebGL rendering context at
    /// once, per window. Browsers cap live WebGL contexts at ~8-16 per
    /// process and silently kill the oldest beyond that; the flow grid can
    /// keep ~18-30 panes live. Consumed by the frontend's WebGL LRU.
    #[serde(default = "default_max_webgl_panes")]
    pub max_webgl_panes: u32,
    /// Flush cadence (ms) for the `background` emission tier — a pane that
    /// is mounted but not the one the operator is looking at. Chunks arriving
    /// inside the window accumulate and leave as one `terminal-output` event.
    /// `0` means no coalescing (every chunk leaves as it arrives); the
    /// [`crate::terminal::visibility::BACKGROUND_HOLD_BYTE_CAP`] early flush
    /// applies at every setting. Read through
    /// [`Self::background_flush_interval`] by `TerminalSession::spawn`.
    #[serde(default = "default_background_flush_interval_ms")]
    pub background_flush_interval_ms: u64,
    /// Flush cadence (ms) for the `unwatched` emission tier — a session with
    /// no mounted pane at all. `0` (the default) means **no webview emit**:
    /// output accumulates in the scrollback ring and replays on reveal, and
    /// state tracking is fed by the `terminal-activity` digest instead.
    /// A positive value emits at that cadence instead, and the digest stands
    /// down for that session so the page tap does not double-count it. Read
    /// through [`Self::unwatched_flush_interval`] by `TerminalSession::spawn`.
    #[serde(default)]
    pub unwatched_flush_interval_ms: u64,
    /// Per-session scrollback ring capacity in bytes. This ring is the
    /// source of truth that makes visibility-tiered emission lossless, so
    /// shrinking it trades replay fidelity for memory (plan §5 argues
    /// against shrinking it; the knob exists for operators who have
    /// measured otherwise). Clamped to [`MIN_SCROLLBACK_CAPACITY`] at use.
    #[serde(default = "default_scrollback_capacity_bytes")]
    pub scrollback_capacity_bytes: usize,
    /// Cadence (ms) of the two full-fleet grid scanners (auto-response and
    /// usage-limit detection). Floored at [`MIN_GRID_SCAN_INTERVAL_MS`].
    /// The per-scanner env vars
    /// (`QONTINUI_AUTO_RESPONSE_SCAN_INTERVAL_MS`,
    /// `QONTINUI_USAGE_LIMIT_SCAN_INTERVAL_MS`) still win over this value —
    /// they remain the higher-precedence escape hatch.
    #[serde(default = "default_grid_scan_interval_ms")]
    pub grid_scan_interval_ms: u64,
    /// Default `Intent::share_output` for terminal sessions mirrored into
    /// coord. This is a **config/correctness** knob, not a performance one:
    /// no per-terminal coord output pipe exists (`output_pipe: None` on both
    /// terminal registration paths), so turning it off changes what the
    /// intent JSON declares to coord and — via
    /// [`crate::session::intent::Intent::effective_redact_secrets`] — the
    /// default for secret redaction. Default `true`, matching the value
    /// that used to be hardcoded at both terminal create sites.
    #[serde(default = "default_share_terminal_output")]
    pub share_terminal_output: bool,
    /// Explicit `Intent::redact_secrets` for terminal sessions. `None` (the
    /// default) keeps the historical behavior: redaction follows
    /// `share_terminal_output`. `Some(true)`/`Some(false)` pins it.
    #[serde(default)]
    pub redact_terminal_secrets: Option<bool>,
    /// Open-session count past which the terminal page shows a **dismissible
    /// advisory banner**. WARN ONLY — nothing consults this value to block,
    /// throttle or refuse a spawn, and nothing may (plan §5).
    #[serde(default = "default_max_sessions_warn")]
    pub max_sessions_warn: u32,
}

/// Floor for [`PerformanceSettings::grid_scan_interval_ms`]. Mirrors the
/// floor the env-var overrides have always enforced.
pub const MIN_GRID_SCAN_INTERVAL_MS: u64 = 200;

/// Floor for [`PerformanceSettings::scrollback_capacity_bytes`] (64 KiB).
/// Below this the ring stops being a usable replay source for a reveal.
pub const MIN_SCROLLBACK_CAPACITY: usize = 64 * 1024;

fn default_max_webgl_panes() -> u32 {
    8
}

/// The `background` tier's historical spacing, so "no `performance` key in
/// settings.json" and "the constant the tier always used" cannot drift apart —
/// the same tie `default_scrollback_capacity_bytes` makes.
fn default_background_flush_interval_ms() -> u64 {
    crate::terminal::visibility::BACKGROUND_FLUSH_INTERVAL.as_millis() as u64
}

/// The reader thread's historical ring size, so "no `performance` key in
/// settings.json" and "the constant the reader always used" cannot drift
/// apart.
fn default_scrollback_capacity_bytes() -> usize {
    crate::terminal::session::SCROLLBACK_CAPACITY
}

fn default_grid_scan_interval_ms() -> u64 {
    1500
}

fn default_share_terminal_output() -> bool {
    true
}

fn default_max_sessions_warn() -> u32 {
    30
}

impl Default for PerformanceSettings {
    /// Round-trips through an empty JSON object for the same reason
    /// [`Settings::default`] does — a derived `Default` would ignore every
    /// `#[serde(default = "fn")]` above and silently zero the caps.
    fn default() -> Self {
        serde_json::from_str("{}").expect(
            "PerformanceSettings must deserialize from an empty JSON object — every field needs a serde default",
        )
    }
}

impl PerformanceSettings {
    /// Grid-scan interval with the floor applied.
    pub fn effective_grid_scan_interval_ms(&self) -> u64 {
        self.grid_scan_interval_ms.max(MIN_GRID_SCAN_INTERVAL_MS)
    }

    /// Scrollback ring capacity with the floor applied.
    pub fn effective_scrollback_capacity(&self) -> usize {
        self.scrollback_capacity_bytes.max(MIN_SCROLLBACK_CAPACITY)
    }

    /// The `background` tier's flush spacing, as the emission path wants it.
    ///
    /// No floor, deliberately: unlike the grid scanners and the scrollback
    /// ring, a too-small value here cannot starve anything or allocate — `0`
    /// simply means "no coalescing", which is a coherent (if unhelpful)
    /// choice, and the byte cap still bounds a single event either way. The
    /// settings panel declares `min: 0` for the same reason.
    pub fn background_flush_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.background_flush_interval_ms)
    }

    /// The `unwatched` tier's flush spacing, or `None` when the tier emits
    /// nothing to the webview at all.
    ///
    /// `0` is not "flush immediately" here — it is the tier's documented OFF
    /// switch and the stock default, which is why this returns an `Option`
    /// rather than a bare `Duration`. Collapsing the two would turn every
    /// stock install's silent `unwatched` tier into an uncoalesced firehose,
    /// i.e. the exact regression Phase 5 exists to prevent.
    pub fn unwatched_flush_interval(&self) -> Option<std::time::Duration> {
        match self.unwatched_flush_interval_ms {
            0 => None,
            ms => Some(std::time::Duration::from_millis(ms)),
        }
    }
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
/// The canonical definition now lives in `qontinui-schemas`
/// (`qontinui_types::projects::SavedProject`) so the TypeScript and Python
/// bindings are generated rather than hand-mirrored — the struct grew from 4
/// fields to 16 with the Projects dashboard, which is exactly where a
/// hand-mirror starts lying. Re-exported here so `settings::SavedProject`
/// keeps working for every existing consumer.
pub use qontinui_types::projects::SavedProject;

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

/// Which arm produced the app config directory.
///
/// The house `(value, source)` shape (`profiles::CoordBaseSource`,
/// `api_config::ApiBaseUrlArm`) applied to the directory that decides where
/// `settings.json` — and therefore most of the rest of this runner's
/// configuration — is read from. `config_report`'s layer 2 asks this resolver
/// for the arm instead of re-reading the env var itself, so the report cannot
/// carry a second, drifting copy of the override rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigDirSource {
    /// Env `QONTINUI_CONFIG_DIR` was set to a NON-EMPTY value (the per-instance
    /// override the supervisor sets for spawned runners).
    ///
    /// The emptiness filter is load-bearing, and `profiles::settings_json_path`
    /// — the lib-side resolver of the same file — now applies it too. It did
    /// not always: an exported-but-empty variable took this resolver to the
    /// platform dir and that one to a CWD-relative path, which became a
    /// data-loss-shaped bug the moment the lib gained a tier WRITER. See
    /// `profiles::SettingsJsonPathSource::EnvConfigDir`.
    EnvConfigDir,
    /// No usable `QONTINUI_CONFIG_DIR`; the platform config dir +
    /// `com.qontinui.runner`.
    PlatformConfigDir,
}

impl ConfigDirSource {
    /// Stable wire string.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ConfigDirSource::EnvConfigDir => "env:QONTINUI_CONFIG_DIR",
            ConfigDirSource::PlatformConfigDir => "platform_config_dir",
        }
    }
}

/// Resolve the app config directory + WHICH arm produced it, **creating
/// nothing**. This is the ONE definition of the precedence rule; both
/// [`get_config_dir`] (which additionally ensures the directory exists) and
/// every read-only caller go through it.
///
/// The split exists because "resolve" and "ensure" are different operations and
/// only one of them is safe for an observer. `config_report`'s layer 2 must be
/// able to say *"`QONTINUI_CONFIG_DIR` names `D:/typo` and that directory does
/// NOT exist"* — a diagnostic that called the ensuring variant would silently
/// `create_dir_all` the typo and then report `exists: true`, materializing the
/// thing it is describing and destroying the only evidence of the fault. Same
/// discipline `config_report_cmd::claude_settings_carrier_reading` states for
/// `materialize`.
///
/// The `Err` arm is a genuine "could not resolve", never a fallback: a caller
/// (and `config_report` layer 2) gets an error string naming what failed rather
/// than a plausible-looking directory nothing was actually read from.
pub(crate) fn resolve_config_dir() -> Result<(PathBuf, ConfigDirSource), String> {
    resolve_config_dir_from(
        std::env::var("QONTINUI_CONFIG_DIR").ok(),
        dirs::config_dir(),
    )
}

/// [`resolve_config_dir`] as a PURE function of its two inputs, so the
/// precedence rule — and, load-bearingly, the fact that resolving creates
/// NOTHING — is testable against a path the test controls, with no
/// `set_var("QONTINUI_CONFIG_DIR")` racing every sibling test that reads real
/// settings.
pub(crate) fn resolve_config_dir_from(
    env_config_dir: Option<String>,
    platform_config_dir: Option<PathBuf>,
) -> Result<(PathBuf, ConfigDirSource), String> {
    env_config_dir
        .filter(|s| !s.is_empty())
        .map(|s| (PathBuf::from(s), ConfigDirSource::EnvConfigDir))
        .or_else(|| {
            platform_config_dir.map(|d| {
                (
                    d.join("com.qontinui.runner"),
                    ConfigDirSource::PlatformConfigDir,
                )
            })
        })
        .ok_or_else(|| "Failed to get config directory".to_string())
}

/// [`resolve_config_dir`] **plus** `create_dir_all` — for callers that are about
/// to WRITE into the directory. Never call this from a read or a diagnostic; use
/// [`resolve_config_dir`] there.
pub(crate) fn get_config_dir() -> Result<(PathBuf, ConfigDirSource), String> {
    let (app_data_dir, source) = resolve_config_dir()?;

    // Create directory if it doesn't exist
    if !app_data_dir.exists() {
        fs::create_dir_all(&app_data_dir)
            .map_err(|e| format!("Failed to create app data directory: {}", e))?;
    }

    Ok((app_data_dir, source))
}

/// The settings file path, **creating nothing** — [`resolve_config_dir`] joined
/// with [`SETTINGS_FILE`].
///
/// `pub(crate)` so `config_report` layer 1 can name the file whose provenance it
/// is reporting. Callers must use THIS helper (or [`get_settings_path`]) rather
/// than joining a directory with [`SETTINGS_FILE`] themselves: a second copy of
/// the join would agree today and start lying the first time the real one moved,
/// which is the defect class that whole report exists to expose.
pub(crate) fn resolve_settings_path() -> Result<PathBuf, String> {
    let (dir, _config_dir_source) = resolve_config_dir()?;
    Ok(dir.join(SETTINGS_FILE))
}

/// [`resolve_settings_path`] **plus** the `create_dir_all` of its parent — for
/// callers about to write the file ([`save_settings`]).
pub(crate) fn get_settings_path() -> Result<PathBuf, String> {
    let (dir, _config_dir_source) = get_config_dir()?;
    Ok(dir.join(SETTINGS_FILE))
}

// ============================================================================
// Settings provenance — "unknown" must never collapse into "denied"
// ============================================================================
//
// `load_settings()` used to return `Settings::default()` on EVERY failure
// (path resolution, read error, parse error) with only an `error!` log. Since
// `RunnerTier` defaults to `Local`, a transient Windows sharing violation or a
// truncated settings.json silently demoted a Tier 2 install to a local guest,
// blanked `web_integration.runner_token`, turned off `cloud_sync_enabled`, and
// reset `setup_completed` — re-showing the first-run SetupWizard. Worse, the
// defaulted struct was then PERSISTED by the tier migration, making the
// demotion permanent, and any unrelated `load → mutate one field → save` did
// the same on the next checkbox toggle.
//
// The fix is to make the failure legible: every load now carries a
// [`SettingsProvenance`] saying whether the returned `Settings` reflect real
// on-disk user state. Nothing that writes identity-bearing state may act on a
// non-authoritative load.

/// Where an in-memory [`Settings`] value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsProvenance {
    /// A `settings.json` existed and parsed successfully. The values are the
    /// user's real, persisted state.
    Loaded,
    /// No `settings.json` existed — a genuine first run. Defaults ARE the
    /// user's state (there is nothing to lose), so this is authoritative.
    FreshInstall,
    /// A `settings.json` (or its directory) existed but could not be read or
    /// parsed. The accompanying `Settings` is a DEFAULT placeholder: its
    /// identity-bearing fields (`tier`, `web_integration.runner_token`,
    /// `setup_completed`, `qontinui_user_id`, sync toggles) are **not** the
    /// user's values and must never be persisted, nor used to deny a
    /// capability.
    Unreadable,
}

impl SettingsProvenance {
    /// `true` when the accompanying `Settings` reflects real user state and may
    /// safely be written back to disk / used to gate capability.
    pub fn is_authoritative(self) -> bool {
        matches!(self, Self::Loaded | Self::FreshInstall)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::FreshInstall => "fresh_install",
            Self::Unreadable => "unreadable",
        }
    }
}

/// A settings load together with the provenance of its values.
///
/// Deliberately NOT `Clone`/`Debug`: `Settings` is neither, and a whole-settings
/// clone is exactly the pattern this type exists to discourage.
pub struct LoadedSettings {
    pub settings: Settings,
    pub provenance: SettingsProvenance,
    /// Human-readable reason the load was non-authoritative
    /// (`None` for `Loaded` / `FreshInstall`).
    pub error: Option<String>,
}

impl LoadedSettings {
    pub fn is_authoritative(&self) -> bool {
        self.provenance.is_authoritative()
    }

    /// The canonical user-facing message for an unreadable settings file.
    /// Deliberately names the remediation (fix/move the file) rather than
    /// implying the user is signed out or on a lower tier.
    pub fn unreadable_message(&self) -> String {
        format!(
            "settings.json could not be read ({}) — the runner cannot determine your \
             saved configuration. Nothing was changed. Fix or move {} and retry.",
            self.error.as_deref().unwrap_or("unknown error"),
            settings_path_for_display(),
        )
    }
}

fn settings_path_for_display() -> String {
    resolve_settings_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| SETTINGS_FILE.to_string())
}

/// A recorded settings-read fault, exposed so the UI can surface a loud
/// "settings unreadable" banner instead of silently rendering a demoted app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsFault {
    pub path: String,
    pub error: String,
    /// Unix seconds when the fault was last observed.
    pub detected_at_unix: u64,
}

/// Process-global last-known settings-read fault. `None` once a load succeeds
/// again, so the surface self-heals when the file becomes readable.
static SETTINGS_FAULT: std::sync::RwLock<Option<SettingsFault>> = std::sync::RwLock::new(None);

/// Process-global, in-memory-only tier override.
///
/// Set by [`crate::commands::auth::set_runner_tier`] on a runner that must not
/// write the shared `settings.json` (a secondary — any supervisor-launched
/// temp/named instance; see the FOOTGUN GUARD in [`load_settings_full`]). Before
/// this existed, that branch logged "applying in-memory only" and then applied
/// NOTHING: the command was a total no-op, `get_runner_tier` kept answering the
/// old tier, and every tier-dependent state transition (notably the frontend's
/// `isTier2` flip) was untestable on a temp runner — a green result there was
/// vacuous.
///
/// It is applied as the LAST overlay in [`load_settings_full`], so an explicit
/// runtime choice beats the spawn-time `QONTINUI_RUNNER_TIER` env overlay. It is
/// never read by [`update_settings`] (which starts from the raw on-disk
/// document), so it can never leak into a persisted file.
static TIER_OVERRIDE: std::sync::RwLock<Option<RunnerTier>> = std::sync::RwLock::new(None);

/// Apply a tier for the remainder of this process's lifetime WITHOUT touching
/// any settings file. See [`TIER_OVERRIDE`].
pub fn set_in_memory_tier(tier: RunnerTier) {
    if let Ok(mut guard) = TIER_OVERRIDE.write() {
        *guard = Some(tier);
    }
}

/// The in-memory tier override, if one was set. See [`TIER_OVERRIDE`].
///
/// `pub(crate)` so `redeem_pair_code` can honour the precedence rule when the
/// shared tier writer refuses a secondary: an explicit runtime choice beats an
/// inferred promotion, so the overlay is applied only when none is set.
pub(crate) fn in_memory_tier() -> Option<RunnerTier> {
    TIER_OVERRIDE.read().ok().and_then(|g| *g)
}

fn record_settings_fault(fault: Option<SettingsFault>) {
    // `load_settings()` runs on hot paths (the relay loop re-reads every
    // iteration), and the overwhelmingly common case is "healthy, still
    // healthy". Take the cheap read lock first and skip the write entirely
    // when there is nothing to clear.
    if fault.is_none() {
        if let Ok(guard) = SETTINGS_FAULT.read() {
            if guard.is_none() {
                return;
            }
        }
    }
    if let Ok(mut guard) = SETTINGS_FAULT.write() {
        *guard = fault;
    }
}

/// The most recent settings-read fault, or `None` when the last load was
/// authoritative. Read by the `get_settings_health` command and the
/// `/web-integration/status` diagnostic.
pub fn settings_fault() -> Option<SettingsFault> {
    SETTINGS_FAULT.read().ok().and_then(|g| g.clone())
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One cached parse of the settings document, validated against the file's
/// identity metadata. Both fields are compared: mtime alone is not a safe
/// invalidation signal on Windows (FAT-derived volumes and same-tick rewrites
/// can repeat a timestamp), and a length change catches the common
/// edit-in-place case that a repeated timestamp would hide. In-process writes
/// do not rely on either — [`save_settings`] drops the entry outright.
struct CachedSettings {
    mtime: std::time::SystemTime,
    len: u64,
    settings: Settings,
}

/// mtime-checked cache of the parsed settings document, keyed by path.
///
/// Terminal spawn read + double-parsed `settings.json` on every open
/// (`terminal/session.rs` → `get_ai_settings` → here) and `ClaudeSession::spawn`
/// did it a second, independent time — so N concurrent opens paid 2N full JSON
/// parses of the whole document for values that change only when the operator
/// edits them. Follows the in-repo precedent at
/// `crate::terminal::transcript`'s `CachedSession`.
static SETTINGS_CACHE: once_cell::sync::Lazy<std::sync::Mutex<HashMap<PathBuf, CachedSettings>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

/// Drop every cached parse. Called after any in-process write so a
/// read-modify-write never observes its own pre-write document, independent of
/// filesystem timestamp granularity.
fn invalidate_settings_cache() {
    if let Ok(mut c) = SETTINGS_CACHE.lock() {
        c.clear();
    }
}

/// Read + parse the persisted settings document, WITHOUT any env overlays,
/// roster overlay, or tier migration. This is the raw on-disk truth plus its
/// provenance — the base every read-modify-write must start from.
///
/// Served from an mtime+size-validated cache ([`SETTINGS_CACHE`]) when the file
/// has not changed since the last successful parse. Only an AUTHORITATIVE
/// `Loaded` result is ever cached — a fresh install, an unresolvable path and
/// an unreadable/corrupt file all re-check the disk every call, so the
/// "settings unreadable" banner still clears the instant the file is fixed.
///
/// # This function does not WRITE, and that is a contract
///
/// It resolves through [`resolve_settings_path`] (which creates no directory),
/// it never runs the tier / `local_user_id` migration, it never calls
/// [`save_settings`], it never touches `claude-accounts.json`, and it never
/// reaches the OS keyring. [`load_settings_full`] does all four, which is why
/// `config_report`'s layer 1 asks THIS function instead: a diagnostic that
/// mints a `local_user_id` UUID into the operator's real `settings.json` has
/// changed the answer by asking the question. `pub(crate)` exists for exactly
/// that caller.
///
/// The one process-global effect it retains is [`record_settings_fault`], which
/// stores the CURRENT truth about the file (or clears it) for the UI banner —
/// idempotent with respect to reality, and the same value any concurrent load
/// would record.
pub(crate) fn read_settings_from_disk() -> LoadedSettings {
    let path = match resolve_settings_path() {
        Ok(p) => p,
        Err(e) => {
            let error = format!("cannot resolve settings path: {e}");
            error!("Settings unreadable — refusing to synthesize defaults: {error}");
            record_settings_fault(Some(SettingsFault {
                path: settings_path_for_display(),
                error: error.clone(),
                detected_at_unix: now_unix(),
            }));
            return LoadedSettings {
                settings: Settings::default(),
                provenance: SettingsProvenance::Unreadable,
                error: Some(error),
            };
        }
    };
    read_settings_from_path(&path)
}

/// [`read_settings_from_disk`] against an explicit path. Split out so the cache
/// behaviour is testable against a tempdir without touching the process-global
/// `QONTINUI_CONFIG_DIR` (which would race every sibling test that reads real
/// settings).
fn read_settings_from_path(path: &std::path::Path) -> LoadedSettings {
    let unreadable = |error: String| {
        error!("Settings unreadable — refusing to synthesize defaults: {error}");
        record_settings_fault(Some(SettingsFault {
            path: settings_path_for_display(),
            error: error.clone(),
            detected_at_unix: now_unix(),
        }));
        LoadedSettings {
            settings: Settings::default(),
            provenance: SettingsProvenance::Unreadable,
            error: Some(error),
        }
    };

    // ONE stat answers both "does it exist" and "is the cache still valid" —
    // the old `path.exists()` was already a stat, so this adds no syscall.
    let (mtime, len) = match fs::metadata(path) {
        Ok(md) => (md.modified().ok(), md.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            record_settings_fault(None);
            return LoadedSettings {
                settings: Settings::default(),
                provenance: SettingsProvenance::FreshInstall,
                error: None,
            };
        }
        Err(e) => return unreadable(format!("stat failed: {e}")),
    };

    if let Some(mtime) = mtime {
        if let Ok(cache) = SETTINGS_CACHE.lock() {
            if let Some(hit) = cache.get(path) {
                if hit.mtime == mtime && hit.len == len {
                    record_settings_fault(None);
                    return LoadedSettings {
                        settings: hit.settings.clone(),
                        provenance: SettingsProvenance::Loaded,
                        error: None,
                    };
                }
            }
        }
    }

    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return unreadable(format!("read failed: {e}")),
    };

    match serde_json::from_str::<Settings>(&contents) {
        Ok(mut s) => {
            // Gate-2 split migration: carry a pre-split explicit
            // `cloud_sync_enabled` decision forward onto the new
            // `session_metadata_sync_enabled` flag when the new key is absent.
            // Only runs against a real, successfully-parsed settings file — a
            // genuinely fresh install never reaches this branch and already
            // gets the field's own serde default (`true`) from the parse above.
            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&contents) {
                migrate_metadata_sync_flag(&raw, &mut s);
                migrate_tier_chosen_explicitly(&raw, &mut s);
            }
            record_settings_fault(None);
            // Cache the POST-migration document: the migration is a pure
            // function of the file bytes, so a cache hit must be
            // indistinguishable from a fresh parse.
            if let Some(mtime) = mtime {
                if let Ok(mut cache) = SETTINGS_CACHE.lock() {
                    cache.insert(
                        path.to_path_buf(),
                        CachedSettings {
                            mtime,
                            len,
                            settings: s.clone(),
                        },
                    );
                }
            }
            LoadedSettings {
                settings: s,
                provenance: SettingsProvenance::Loaded,
                error: None,
            }
        }
        // BOUNDED, deliberately not `{e}`. `serde_json::Error`'s Display for a
        // DATA error (`invalid type`, `invalid value`) QUOTES THE OFFENDING
        // VALUE OUT OF THE FILE — and this file carries
        // `web_integration.runner_token` and `qontinui_user_id`. That string
        // reaches the user-facing `unreadable_message()` banner and
        // `config_report`'s layer 1, so the category + position is the whole of
        // what may cross: it is everything a reader needs to fix the file, and
        // it cannot carry a credential.
        Err(e) => unreadable(format!(
            "parse failed: JSON {:?} error at line {} column {}",
            e.classify(),
            e.line(),
            e.column()
        )),
    }
}

/// Load settings from file.
///
/// Convenience wrapper over [`load_settings_full`] for the many read-only
/// callers that only want values and cannot act on provenance. **Any caller
/// that writes settings back, or that turns a settings value into a
/// capability/authentication verdict, must use [`load_settings_full`] (or
/// [`update_settings`] / [`resolve_tier`]) instead** — see the provenance
/// module docs above.
pub fn load_settings() -> Settings {
    load_settings_full().settings
}

/// Web-integration env-var overrides (Phase 3G) — the ONE definition.
///
/// In-memory overlay only; never persisted to disk. If either variable is set
/// via env and a non-empty value is present in settings, the env wins. If both
/// env vars are set and the persisted `enabled` flag is false, default to
/// enabled (headless deploys shouldn't have to save settings to activate web
/// integration).
///
/// Extracted out of [`load_settings_full`] so a caller holding a document from
/// the NON-MUTATING [`read_settings_from_disk`] can reach the same
/// `web_integration` values the full loader would have produced without paying
/// that loader's writes (`claude-accounts.json`, a minted `local_user_id`, a
/// `save_settings` of the operator's real file, an OS-keyring read).
/// `config_report_cmd::settings_derived_inputs` is that caller, and
/// `api_config::api_base_url_inputs_from` documents why those two fields are all
/// its rung needs. Restating this overlay at the call site instead would be a
/// second copy of a precedence rule — the defect class the config report exists
/// to expose.
///
/// The whitespace case is the one that makes this an extraction rather than a
/// convenience: `QONTINUI_WEB_BACKEND_URL="  "` is non-empty HERE (so it
/// overwrites `backend_url`) while `resolve_api_base_url` treats it as unset. A
/// reader that skipped this overlay would resolve the DISK url on that machine
/// and the runner would resolve the build default.
pub(crate) fn apply_web_integration_env_overlay(settings: &mut Settings) {
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
}

#[cfg(test)]
thread_local! {
    /// TEST-ONLY entry counter for [`load_settings_full`], **per thread**.
    ///
    /// [`load_settings_full`] is the runner's only settings
    /// WRITER-by-side-effect: it runs `claude_accounts::load_with_migration()`
    /// (writing `claude-accounts.json`), can mint a `local_user_id` UUID and
    /// call [`save_settings`] on the operator's real file, and reaches the OS
    /// keyring. `config_report` is required never to reach it, and the
    /// fingerprint test that was supposed to enforce that passes vacuously on
    /// any machine where boot already ran the one-shot migration
    /// (`needs_persist` false, `MIGRATE_ONCE` fired) — which is every dev box,
    /// so it could only ever pass.
    ///
    /// Counting entries makes the invariant directly falsifiable without the two
    /// things that would make the test unusable: a process-global
    /// `set_var("QONTINUI_CONFIG_DIR")` (which races every sibling test that
    /// reads real settings — the documented cause of an existing flake) and a
    /// process-global counter (which every parallel test calling this function
    /// would perturb). A `thread_local!` is immune to both: each `#[test]` runs
    /// on its own thread, so the count a test observes is exactly what that
    /// test's own call graph did.
    ///
    /// Read it with [`settings_full_load_count`]; the assertion lives in
    /// `config_report_cmd::tests::config_report_never_reaches_the_settings_writer`.
    static SETTINGS_FULL_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times [`load_settings_full`] has been entered on THIS thread. See
/// [`SETTINGS_FULL_LOADS`].
#[cfg(test)]
pub(crate) fn settings_full_load_count() -> usize {
    SETTINGS_FULL_LOADS.with(|c| c.get())
}

/// Load settings from file, reporting whether the values are the user's real
/// persisted state ([`SettingsProvenance`]).
pub fn load_settings_full() -> LoadedSettings {
    #[cfg(test)]
    SETTINGS_FULL_LOADS.with(|c| c.set(c.get() + 1));

    let LoadedSettings {
        mut settings,
        provenance,
        error,
    } = read_settings_from_disk();

    // The document EXACTLY as the file has it, captured before the first
    // overlay touches it. Every persist below is built from THIS, never from
    // the overlaid view — see [`document_to_persist`], which is where the
    // argument lives. Unconditional because the overlays mutate `settings` in
    // place and the decision to persist is only reached afterwards.
    let on_disk = settings.clone();

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

    apply_web_integration_env_overlay(&mut settings);

    // Machine-global Claude account roster overlay (claude-accounts.json).
    // The roster (config dirs, selection mode, manual config_dir pin, launch
    // commands) is a machine-global fact; the copies in per-instance
    // settings.json files are stale shadows kept alive by whole-`Settings`
    // saves. When the machine-global file exists it wins UNCONDITIONALLY for
    // all four fields, for every instance (primary + temp + named). When it
    // is absent, per-instance values load untouched (legacy behavior). Also
    // runs the one-shot seed migration from the unscoped settings.json. See
    // the module doc in `claude_accounts.rs` for the last-writer-wins model.
    if let Some(roster) = crate::claude_accounts::load_with_migration() {
        crate::claude_accounts::apply_roster_overlay(&mut settings, roster);
    }

    // Tier + local_user_id migration / lazy init.
    //
    // Both branches may mutate the in-memory `settings` and request a
    // persist. The persist is best-effort (logged on failure) — an in-memory
    // value is still correct for the rest of this process's lifetime.
    //
    // FOOTGUN GUARD: the persist below writes whatever file `get_settings_path`
    // resolves. That path IS instance-scoped when `QONTINUI_CONFIG_DIR` is set
    // (the supervisor sets it per spawned instance — `manager.rs` spawn env —
    // so supervisor-spawned temp/named runners write their own file), but a
    // secondary launched with only `QONTINUI_INSTANCE_NAME` (no
    // `QONTINUI_CONFIG_DIR`) resolves the primary's SHARED
    // `dirs::config_dir()/com.qontinui.runner/settings.json`. A secondary has
    // no `runner_token`, so `migrate_tier_in_place` infers `tier=Local` for
    // it. If such a secondary persisted into the shared file, it would
    // silently overwrite the primary's persisted Tier 2 (`qontinui_account`)
    // state on disk, demoting the primary the next time it loads from
    // `local`. Therefore: ONLY the primary runner may persist a
    // tier/local_user_id migration. Secondaries (temp + named, i.e. any
    // runner the supervisor launched with `QONTINUI_INSTANCE_NAME`) keep the
    // migration in-memory only — correct for this process's lifetime, never
    // written to a settings file. This mirrors the in-memory-only
    // `QONTINUI_RUNNER_TIER` overlay just below.
    //
    // C2 GUARD: a `Settings::default()` synthesized because the real file was
    // unreadable has `tier_initialized == false` and an empty `runner_token`,
    // so `migrate_tier_in_place` would stamp `tier = Local, tier_initialized =
    // true` and request a persist — atomically overwriting the corrupt (but
    // possibly recoverable) file with an all-defaults one and logging it as a
    // success. That is silent data destruction, and it makes the demotion
    // permanent across restarts. A non-authoritative load therefore skips the
    // migration ENTIRELY: no in-memory tier stamp, no local_user_id mint, no
    // persist.
    let mut needs_persist = false;
    // How the load-time tier migration classified itself. `ProcessLocal` means
    // the promotion is applied in memory and kept out of every write, including
    // one taken for another reason — see `TierMigration` and
    // `document_to_persist`.
    let mut tier_migration = TierMigration::Unchanged;
    if provenance.is_authoritative() {
        // The tier inference's THREE probes, all taken here so
        // `migrate_tier_in_place` stays a pure, fully testable helper:
        //
        // * `QONTINUI_SERVER_MODE`, through the launch-env module that owns
        //   the single parse of it;
        // * device pairing, through the lib's `paired_user.json` reader. One
        //   small file read — deliberately NOT a credential-store read, which
        //   can block on an OS keychain unlock and must never happen on a
        //   settings load (see `pair::device_is_paired` for the full argument);
        // * whether the FILE carries a `web_integration.runner_token`. Read off
        //   `on_disk`, because `apply_web_integration_env_overlay` above may
        //   have put `QONTINUI_RUNNER_TOKEN` into `settings` — a runtime-only
        //   override that must promote this process without promoting the
        //   install. See `migrate_tier_in_place`'s `disk_runner_token`.
        tier_migration = migrate_tier_in_place(
            &mut settings,
            crate::launch_env::server_mode_from_env(),
            qontinui_runner_lib::pair::device_is_paired(),
            !on_disk.web_integration.runner_token.trim().is_empty(),
        );
        if tier_migration.persists() {
            needs_persist = true;
        }
        if settings.local_user_id.trim().is_empty() {
            settings.local_user_id = uuid::Uuid::new_v4().to_string();
            needs_persist = true;
        }
    } else {
        error!(
            "Settings load was non-authoritative ({}) — skipping the tier/local_user_id \
             migration and its persist so the existing settings.json is not overwritten \
             with defaults",
            provenance.as_str()
        );
    }
    let is_secondary = crate::instance::is_secondary();
    if needs_persist && is_secondary {
        // ONCE per process, at info!. This branch re-runs on every settings
        // load (the relay loop re-reads every iteration), so it used to emit
        // ~30 identical lines per 500-entry log window and bury real signal
        // during debugging. The fact is process-invariant — a secondary never
        // becomes a primary — so repeating it carries no information.
        static LOGGED: std::sync::Once = std::sync::Once::new();
        let mut first = false;
        LOGGED.call_once(|| first = true);
        if first {
            info!(
                "Skipping tier/local_user_id migration persist for secondary runner \
                 (instance={:?}) — would clobber the primary's shared settings.json; \
                 keeping the migration in-memory only (tier={:?}). Logged once per process.",
                crate::instance::instance_name(),
                settings.tier
            );
        } else {
            tracing::debug!(
                "Skipping tier/local_user_id migration persist for secondary runner (tier={:?})",
                settings.tier
            );
        }
    }
    if should_persist_migration(needs_persist, is_secondary, provenance) {
        let to_persist = document_to_persist(&on_disk, &settings, tier_migration);
        if let Err(e) = save_settings(&to_persist) {
            error!("Failed to persist tier/local_user_id migration: {}", e);
        } else {
            info!(
                "Persisted tier/local_user_id migration (tier={:?}, local_user_id set)",
                to_persist.tier
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

    // Runtime in-memory tier override (`set_runner_tier` on a secondary).
    // Applied LAST so an explicit runtime choice beats the spawn-time env
    // overlay above — the operator/driver picked this tier after boot.
    // In-memory only; `update_settings` reads the raw on-disk document, so
    // this can never reach a settings file.
    apply_in_memory_tier_overlay(&mut settings, in_memory_tier());

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

    LoadedSettings {
        settings,
        provenance,
        error,
    }
}

// ============================================================================
// Provenance-checked write path (C3)
// ============================================================================

/// Read-modify-write a single settings field WITHOUT the whole-file clobber
/// that `load_settings() → mutate one field → save_settings()` invites.
///
/// Two guarantees over the old pattern:
///
/// 1. **Hard-errors on a non-authoritative base.** If the settings file could
///    not be read or parsed, the mutation is refused with the reason instead of
///    persisting an all-defaults struct — which used to sign the runner out of
///    Tier 2 as a side effect of toggling an unrelated checkbox, and report
///    success while doing it.
/// 2. **Mutates the PERSISTED document, not the env-overlaid view.**
///    `load_settings()` layers in-memory-only overlays (`QONTINUI_RUNNER_TIER`,
///    `QONTINUI_RUNNER_TOKEN`, Restate ports, the machine-global Claude
///    roster). Saving that view wrote those overlays into the file — e.g. a
///    secondary launched with `QONTINUI_RUNNER_TIER=local` persisted
///    `tier = Local` over the primary's Tier 2 the first time it saved any
///    unrelated setting. Starting from the raw on-disk document keeps every
///    "in-memory only; never persisted" comment true. The load path's own
///    persist follows the same rule for the same reason — see
///    [`document_to_persist`].
///
/// The closure sees the on-disk document. Callers that need the *effective*
/// (overlaid) values to compute the new one should read them separately with
/// [`load_settings`] before calling.
pub fn update_settings<F>(mutate: F) -> Result<(), String>
where
    F: FnOnce(&mut Settings),
{
    let loaded = read_settings_from_disk();
    if !loaded.is_authoritative() {
        let msg = loaded.unreadable_message();
        error!("update_settings refused: {msg}");
        return Err(msg);
    }
    let mut settings = loaded.settings;
    mutate(&mut settings);
    save_settings(&settings)
}

/// Tri-state runner tier: a settings-read failure means the tier is UNKNOWN,
/// not `Local`. Capability gates must render "we could not determine your
/// tier" rather than the lesser capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TierResolution {
    Known(RunnerTier),
    Unknown { reason: String },
}

impl TierResolution {
    /// The tier if known, else `None`. Never guesses.
    ///
    /// Part of the tri-state contract even though the current in-tree callers
    /// all `match` exhaustively — a caller that only needs "the tier, if we
    /// actually know it" must have a way to ask that does not fabricate one.
    #[allow(dead_code)]
    pub fn known(&self) -> Option<RunnerTier> {
        match self {
            Self::Known(t) => Some(*t),
            Self::Unknown { .. } => None,
        }
    }

    /// `"local" | "local_provider" | "qontinui_account" | "unknown"`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Known(RunnerTier::Local) => "local",
            Self::Known(RunnerTier::LocalProvider) => "local_provider",
            Self::Known(RunnerTier::QontinuiAccount) => "qontinui_account",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Resolve the runner tier, distinguishing "definitively Tier 0/1" from
/// "we could not read settings.json, so the tier is unknown".
pub fn resolve_tier() -> TierResolution {
    let loaded = load_settings_full();
    if loaded.is_authoritative() {
        TierResolution::Known(loaded.settings.tier)
    } else {
        TierResolution::Unknown {
            reason: loaded.unreadable_message(),
        }
    }
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

/// Apply the runtime in-memory tier override (`set_runner_tier`, i.e.
/// [`TIER_OVERRIDE`]) as the LAST overlay on `settings.tier`. `None` leaves the
/// settings untouched.
///
/// This is the top of the tier precedence stack. [`load_settings_full`] runs,
/// in order:
///
/// 1. [`migrate_tier_in_place`] — the persisted-state inference, including the
///    headless (`QONTINUI_SERVER_MODE`) default;
/// 2. [`apply_tier_env_overlay`] — the spawn-time `QONTINUI_RUNNER_TIER`;
/// 3. this — the operator's explicit runtime choice, which therefore beats
///    both.
///
/// Factored out of `load_settings_full` for the same reason
/// [`apply_tier_env_overlay`] was: so unit tests can exercise the precedence
/// against a fixture `Settings` without mutating a process-wide global (see
/// `feedback_env_var_tests_serialize`). In-memory only — `update_settings`
/// reads the raw on-disk document, so an overlay can never reach a file.
pub(crate) fn apply_in_memory_tier_overlay(
    settings: &mut Settings,
    override_tier: Option<RunnerTier>,
) {
    if let Some(t) = override_tier {
        settings.tier = t;
        settings.tier_initialized = true;
    }
}

/// Resolve this install's runner tier from its persisted state, in place.
/// Returns `true` if `settings` was changed (caller should persist).
///
/// # One rule, in the lib
///
/// The rule itself is NOT here. It is
/// [`qontinui_runner_lib::profiles::infer_tier`] +
/// [`tier_is_open_to_inference`], because there is a SECOND tier reader —
/// `profiles::read_runner_tier`, the raw `settings.json` parse that
/// `coord_doctor` consults — and it used to carry a hand-mirrored copy of this
/// function's inference. That copy had already drifted (only one of the two
/// ever learned about `QONTINUI_SERVER_MODE`). This function is now the thin
/// wrapper that maps a `Settings` onto the shared rule; the other reader maps
/// a `serde_json::Value` onto the same one.
///
/// [`tier_is_open_to_inference`]: qontinui_runner_lib::profiles::tier_is_open_to_inference
///
/// # It is no longer one-shot
///
/// It used to return early on `tier_initialized`, which made it a one-shot
/// latch: a box that first booted before it was paired was stuck at `Local`
/// forever, and the only exit was a button in a WebView a headless box does
/// not have. The gate is now `tier_chosen_explicitly` — the operator's own
/// choice, and nothing else, is final. See
/// [`Settings::tier_chosen_explicitly`] and `tier_is_open_to_inference` for
/// why the two had to be separated, and why re-inference can only ever
/// promote (never demote) a box that has already been initialized.
///
/// # Why the signals are parameters
///
/// So this stays a pure, fully testable mapping: `tier_matrix_tests` drives
/// every combination with no process env and no temp dir. `QONTINUI_SERVER_MODE`
/// is parsed exactly once in the tree, by
/// [`crate::launch_env::server_mode_from_env`]; `paired` comes from
/// [`qontinui_runner_lib::pair::device_is_paired`]. `load_settings_full` takes
/// both probes and threads them in. Re-reading either inline here would add a
/// second probe site — which is precisely how the `runner_token` inference
/// came to exist twice.
///
/// # This is a product posture default, not a pure bug fix
///
/// Tier 0 (`Local`) advertises "no Qontinui account, no cloud round-trips".
/// The `server_mode` signal makes a headless box default to the **cloud**
/// tier, which is the operator's explicit instruction (plan
/// `2026-08-29-headless-runner-tier-never-reaches-qontinui-account`) and the
/// precondition for driving a remote runner at all.
///
/// It is a default, never a trap, and that rests on TWO properties, not one:
///
/// 1. **Both overlays beat it, in memory.** This inference sits at the BOTTOM
///    of the stack in [`load_settings_full`], with [`apply_tier_env_overlay`]
///    (`QONTINUI_RUNNER_TIER`) and then [`apply_in_memory_tier_overlay`] (a
///    runtime `set_runner_tier`) applied over it, in that order — so the tier
///    every consumer resolves is the operator's, not this one's.
/// 2. **A promotion with no signal ON DISK is never PERSISTED.** In-memory
///    precedence alone would not have been enough: the persist happens BEFORE
///    the overlays are applied, so the escape hatch would have been honoured
///    for one process and overwritten on disk permanently. This function
///    reports [`TierMigration::ProcessLocal`] for that case and
///    [`document_to_persist`] keeps it out of every write. The disk keeps
///    saying what it said.
///
/// A promotion that ALSO had a durable signal (a `runner_token` the FILE
/// carries, pairing) still persists, because that fact is about the install
/// and would be re-derived on the next load anyway. And `set_runner_tier`
/// records `tier_chosen_explicitly`, so an explicit choice closes this
/// function permanently rather than only for one boot.
///
/// # `disk_runner_token` is not `settings.web_integration.runner_token`
///
/// By the time `load_settings_full` calls this, the struct's
/// `web_integration.runner_token` may have come from `QONTINUI_RUNNER_TOKEN`
/// via [`apply_web_integration_env_overlay`] — a documented RUNTIME-ONLY
/// override for headless deploys, not a fact about the install. The struct
/// field is therefore the right input for the INFERENCE (a runner holding that
/// token really is Tier 2 for this process) and the wrong one for the PERSIST
/// CLASSIFICATION. `disk_runner_token` says whether the FILE carried a token,
/// and only that answer may make a promotion durable. Reading the struct for
/// both is exactly how a headless launch used to bake `qontinui_account` into
/// `settings.json` and defeat the `QONTINUI_RUNNER_TIER` escape hatch through
/// the second signal.
pub(crate) fn migrate_tier_in_place(
    settings: &mut Settings,
    server_mode: bool,
    paired: bool,
    disk_runner_token: bool,
) -> TierMigration {
    use qontinui_runner_lib::profiles::{
        infer_tier, tier_is_open_to_inference, InferredTier, TierSignals,
    };

    // A document that has never been initialized has no persisted tier at all,
    // whatever `settings.tier`'s `#[default]` says it is.
    let persisted = settings.tier_initialized.then(|| settings.tier.as_str());
    if !tier_is_open_to_inference(persisted, settings.tier_chosen_explicitly) {
        return TierMigration::Unchanged;
    }

    // The EFFECTIVE token (disk value, possibly overwritten by
    // `QONTINUI_RUNNER_TOKEN`) drives the inference; `disk_runner_token` drives
    // the persist classification. See the doc above.
    let has_runner_token = !settings.web_integration.runner_token.trim().is_empty();
    debug_assert!(
        !disk_runner_token || has_runner_token,
        "a token on disk cannot vanish from the struct — the env overlay only \
         ever overwrites it with a non-empty value"
    );
    let inferred = infer_tier(TierSignals {
        has_runner_token,
        server_mode,
        paired,
    });
    let new_tier = match inferred {
        InferredTier::Local => RunnerTier::Local,
        InferredTier::QontinuiAccount => RunnerTier::QontinuiAccount,
    };

    if settings.tier_initialized && settings.tier == new_tier {
        // Already resolved to exactly this, and the sentinel is set: nothing to
        // write. Keeps `needs_persist` false on the steady-state load, which
        // the relay loop performs on every iteration.
        return TierMigration::Unchanged;
    }

    if settings.tier_initialized {
        // The unlatch firing. `tier_is_open_to_inference` guarantees the
        // persisted tier was `local`, and `new_tier != settings.tier` here, so
        // this branch can only be Local -> QontinuiAccount. A demotion is
        // unreachable by construction, not merely unlikely — silent demotion
        // of a working primary is the top risk in this area.
        //
        // Logged once per process, for the same reason the headless default
        // below is: a SECONDARY never persists the migration, so this branch
        // re-runs on every settings load (the relay loop re-reads on every
        // iteration) and would otherwise bury real signal.
        static UNLATCHED: std::sync::Once = std::sync::Once::new();
        let (token, on_disk, mode, was_paired) =
            (has_runner_token, disk_runner_token, server_mode, paired);
        let to = new_tier.as_str();
        UNLATCHED.call_once(|| {
            info!(
                "runner tier re-inferred from local to {to} (runner_token={token} \
                 [on disk: {on_disk}], server_mode={mode}, paired={was_paired}) — this \
                 install never recorded an explicit tier choice. Pick one in the \
                 SetupWizard's tier step (or set QONTINUI_RUNNER_TIER=local) to pin it."
            );
        });
    } else if server_mode && new_tier == RunnerTier::QontinuiAccount {
        // Logged once per process: on a secondary the migration never
        // persists, so it re-runs on every settings load (the relay loop
        // re-reads every iteration) and would otherwise bury real signal.
        static LOGGED: std::sync::Once = std::sync::Once::new();
        LOGGED.call_once(|| {
            info!(
                "QONTINUI_SERVER_MODE is set and this install has no tier of its own — \
                 defaulting to tier=qontinui_account (the tier that talks to coord). \
                 Set QONTINUI_RUNNER_TIER=local to opt out; it overrides this default."
            );
        });
    }

    settings.tier = new_tier;
    settings.tier_initialized = true;
    // THE PERSIST CLASSIFICATION. A promotion whose firing signals are all
    // properties of THIS PROCESS — `server_mode`, and a `runner_token` that
    // exists only because `QONTINUI_RUNNER_TOKEN` was in the launch
    // environment — describes how the runner was launched, not this install,
    // so it may never reach the disk. See [`TierMigration::ProcessLocal`].
    // Every other outcome is backed by a fact the FILE itself carries (a
    // `web_integration.runner_token` on disk, a `paired_user.json` binding) or
    // is the plain first-boot initialization, and persists as it always did.
    if inferred == InferredTier::QontinuiAccount && !disk_runner_token && !paired {
        debug_assert!(
            server_mode || has_runner_token,
            "with no disk signal, only server_mode or an env-overlaid \
             runner_token can have promoted this"
        );
        TierMigration::ProcessLocal
    } else {
        TierMigration::Durable
    }
}

/// What a load-time tier migration did, and — the part a `bool` could not
/// carry — whether the result may be WRITTEN.
///
/// # Why this is not a bool
///
/// It was, and the bool was wrong in a way that permanently changed operators'
/// boxes. `load_settings_full` persists a migration before it applies the
/// documented `QONTINUI_RUNNER_TIER` escape hatch, so a primary launched
/// `QONTINUI_SERVER_MODE=1 QONTINUI_RUNNER_TIER=local` honoured the opt-out in
/// memory and lost it on disk: the next boot with neither variable set read
/// `qontinui_account`, which `tier_is_open_to_inference` then closes forever.
/// One headless launch flipped the shared `settings.json` a desktop primary
/// reads, for good.
///
/// Reordering the overlay ahead of the persist would not fix it — an
/// unattended headless box sets no `QONTINUI_RUNNER_TIER` at all, and its
/// launch flag would still be baked into the file. The distinction that
/// actually holds is between a signal that is a property of the INSTALL and one
/// that is a property of this PROCESS, and only the migration itself knows
/// which fired. So it reports, and the caller acts.
///
/// `QONTINUI_SERVER_MODE` is not the only process-local signal, and the second
/// one shipped broken: `QONTINUI_RUNNER_TOKEN` is copied into
/// `web_integration.runner_token` by [`apply_web_integration_env_overlay`]
/// BEFORE the migration reads that field, so the identical defect was
/// reachable through the runtime-only token — with a worse tail, because
/// `save_settings` serializes the whole struct and the token itself landed in
/// `settings.json` too. [`migrate_tier_in_place`] therefore classifies on the
/// token the FILE carries, and [`document_to_persist`] builds every write from
/// the raw on-disk document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TierMigration {
    /// Nothing changed: the tier is already resolved, or an explicit choice
    /// closed the inference.
    Unchanged,
    /// The tier changed, and the change is backed by a fact the DISK already
    /// carries — a `web_integration.runner_token` in `settings.json` itself, a
    /// device pairing (`paired_user.json`), or the first-boot initialization of
    /// a tier-less document. Persist it.
    Durable,
    /// The tier changed, but every signal that fired is a property of THIS
    /// PROCESS rather than of the install: `QONTINUI_SERVER_MODE`, and/or a
    /// `web_integration.runner_token` that exists only because
    /// `QONTINUI_RUNNER_TOKEN` was in the launch environment (a documented
    /// runtime-only override — [`apply_web_integration_env_overlay`]). Correct
    /// in memory for this process's lifetime, and never written: see
    /// [`document_to_persist`], which keeps it out of anything that IS written
    /// for another reason (a freshly minted `local_user_id`, say).
    ProcessLocal,
}

impl TierMigration {
    /// Did the in-memory `Settings` change?
    pub(crate) fn changed(self) -> bool {
        !matches!(self, TierMigration::Unchanged)
    }

    /// May the new tier be written to `settings.json`?
    pub(crate) fn persists(self) -> bool {
        matches!(self, TierMigration::Durable)
    }
}

/// The document that may be WRITTEN after a load-time migration: the RAW
/// ON-DISK document, plus exactly the two things the migration is entitled to
/// change — the lazily minted `local_user_id`, and (only when the migration
/// says [`TierMigration::persists`]) `tier` / `tier_initialized`.
///
/// # Why it starts from the on-disk document rather than the loaded one
///
/// `save_settings` serializes the WHOLE struct, and by the time
/// [`load_settings_full`] reaches its persist it has layered three
/// in-memory-only overlays over the parsed document: the `QONTINUI_RESTATE_*`
/// ports/URLs, the `QONTINUI_WEB_BACKEND_URL` / `QONTINUI_RUNNER_TOKEN` pair
/// ([`apply_web_integration_env_overlay`]), and the machine-global Claude
/// roster ([`crate::claude_accounts::apply_roster_overlay`]). Persisting that
/// view wrote every one of them into `settings.json` — including
/// `QONTINUI_RUNNER_TOKEN`, which the headless-deploy docs call a RUNTIME-ONLY
/// override, and which then outlived the process that supplied it. Before the
/// tier unlatch that path was unreachable (an initialized install had nothing
/// to persist); the unlatch made it live.
///
/// [`update_settings`] already solved exactly this for the command path — "the
/// closure sees the on-disk document" — and its own doc claims the property
/// for the whole module: *starting from the raw on-disk document keeps every
/// "in-memory only; never persisted" comment true*. This is that same rule
/// applied to the load path, which is the other writer.
///
/// The roster fields are the one deliberate loss, and they were never
/// load-bearing: `claude-accounts.json` wins UNCONDITIONALLY over the
/// per-instance copy on every load, so those copies are — in that module's own
/// words — stale shadows kept alive by whole-`Settings` saves.
///
/// # Why the tier is rolled back on `ProcessLocal`
///
/// Leaving `needs_persist` false is not enough on its own: a load that mints a
/// `local_user_id` persists for THAT reason and would carry a process-local
/// tier promotion out with it. So the migrated tier is copied across only when
/// the migration classified it durable; otherwise the file keeps the tier it
/// already had, which is what `on_disk` holds.
pub(crate) fn document_to_persist(
    on_disk: &Settings,
    migrated: &Settings,
    tier_migration: TierMigration,
) -> Settings {
    let mut out = on_disk.clone();
    // The lazy `local_user_id` mint — the load path's other reason to write.
    out.local_user_id.clone_from(&migrated.local_user_id);
    if tier_migration.persists() {
        out.tier = migrated.tier;
        out.tier_initialized = migrated.tier_initialized;
    }
    out
}

/// Decide whether a pending tier/local_user_id migration may be persisted to
/// the SHARED settings.json.
///
/// Returns `true` only when ALL THREE hold:
///
/// - there is something to persist (`needs_persist`);
/// - the runner is the primary (`!is_secondary`) — a secondary (temp or named,
///   i.e. any supervisor-launched runner with `QONTINUI_INSTANCE_NAME`) must
///   never write the shared file, because `migrate_tier_in_place` infers
///   `tier=Local` for it (no `runner_token`), which would silently demote the
///   primary's persisted Tier 2 state on disk (see the FOOTGUN GUARD comment
///   in `load_settings_full`);
/// - the base load was **authoritative** (`provenance.is_authoritative()`) — a
///   struct defaulted because the file was unreadable is not the user's state,
///   and writing it destroys the real (possibly recoverable) file. This is the
///   single most important invariant in this module.
///
/// Pure helper (no env / no IO) so the guard can be unit-tested without
/// mutating process env or touching the real settings file.
pub(crate) fn should_persist_migration(
    needs_persist: bool,
    is_secondary: bool,
    provenance: SettingsProvenance,
) -> bool {
    needs_persist && !is_secondary && provenance.is_authoritative()
}

/// Back-fill [`Settings::tier_chosen_explicitly`] on a document written before
/// that field existed.
///
/// `#[serde(default)]` makes every pre-Phase-3 document read "never chose",
/// and for the pairing signal that ambiguity is genuine. For the legacy
/// `web_integration.runner_token` it is not: a document carrying
/// `tier_initialized = true`, `tier = "local"` and a non-empty token cannot
/// have come from any automatic writer, because the old inference would have
/// produced `qontinui_account` from that very token. Only `set_runner_tier`
/// could have written it — the operator who signed in, then picked Local in the
/// SetupWizard to stop the cloud round-trips. Without this back-fill their next
/// upgrade silently re-promotes them, persists it, and brings the relay online.
///
/// The deduction itself lives in
/// [`qontinui_runner_lib::profiles::legacy_tier_choice_is_deducible`], beside
/// the other tier rules, so the lib-side reader (`profiles::read_runner_tier_at`,
/// the one `coord_doctor` consults) applies exactly the same one — the two
/// readers must not drift, which is the whole argument of this area.
///
/// **ABSENT is not the same as present-and-false**, and only the raw `Value`
/// can tell them apart. A `Settings` parsed with `#[serde(default)]` reports
/// `false` for both, so the presence test HAS to be made here, where
/// [`read_settings_from_path`] already holds the raw tree it parses for
/// [`migrate_metadata_sync_flag`] — the established shape for exactly this
/// question. A document that carries the key keeps whatever it says.
pub(crate) fn migrate_tier_chosen_explicitly(raw: &serde_json::Value, settings: &mut Settings) {
    if raw.get("tier_chosen_explicitly").is_some() {
        return;
    }
    if qontinui_runner_lib::profiles::legacy_tier_choice_is_deducible(
        settings.tier_initialized,
        Some(settings.tier.as_str()),
        !settings.web_integration.runner_token.trim().is_empty(),
    ) {
        settings.tier_chosen_explicitly = true;
    }
}

/// Gate-2 split migration (plan `2026-07-10-split-cloud-sync-consent`):
/// carry a pre-split user's explicit `cloud_sync_enabled` decision forward
/// onto the new, independent `session_metadata_sync_enabled` flag.
///
/// - New key already present in the raw JSON → already migrated (or the
///   user explicitly set it post-split); leave `settings` untouched.
/// - New key absent AND legacy `cloud_sync_enabled` present as a bool →
///   carry that value forward. This does not silently grant or revoke
///   anything beyond what the user already decided when the two gates were
///   still one toggle.
/// - Neither key present (genuinely fresh settings content, e.g. `{}`) →
///   leave `settings` untouched; the field's own serde default (`true`)
///   already applied during deserialization.
fn migrate_metadata_sync_flag(raw: &serde_json::Value, settings: &mut Settings) {
    if raw.get("session_metadata_sync_enabled").is_some() {
        return;
    }
    if let Some(legacy) = raw.get("cloud_sync_enabled").and_then(|v| v.as_bool()) {
        settings.session_metadata_sync_enabled = legacy;
    }
}

/// Save settings to file (atomic write to prevent corruption on crash)
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    let path = get_settings_path()?;
    let contents = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    crate::fs_atomic::atomic_write(&path, contents.as_bytes())
        .map_err(|e| format!("Failed to write settings: {}", e))?;

    // Drop the parse cache AFTER the write, so a concurrent reader either sees
    // the old document (already true before this call) or re-parses the new one
    // — never a cached parse that outlives the bytes it came from.
    //
    // This is the WHOLE-DOCUMENT writer, not the only one. The tier writer
    // lives in the lib (`qontinui_runner_lib::profiles::apply_tier_edit_at`,
    // reachable from the `qontinui_profile` bin, which `settings` is not) and
    // edits the JSON tree in place. It cannot touch this cache, so the bin-side
    // door [`promote_tier_to_account`] invalidates for it. Any future in-process
    // writer of `settings.json` owes the same call — the mtime+size heuristic
    // in [`read_settings_from_path`] is a defence against OTHER processes, and
    // it is not sufficient here: a same-tick, same-length rewrite is invisible
    // to it.
    invalidate_settings_cache();

    Ok(())
}

/// The bin-side door to the lib's tier writer: promote this install to
/// `qontinui_account` and then drop the settings parse cache.
///
/// The write itself is [`qontinui_runner_lib::profiles::promote_tier_to_account`],
/// which lives in the lib so the headless `qontinui_profile device pair` door
/// can reach it. That leaves it unable to do the one thing a bin-side write
/// must: this module caches its parse of `settings.json`
/// ([`SETTINGS_CACHE`]), and an in-process write that does not invalidate is
/// visible to a later [`load_settings`] only through the mtime+size heuristic —
/// which [`read_settings_from_path`] documents as insufficient for exactly this
/// case. It happens to work for `"local"` → `"qontinui_account"` because the
/// length changes; that is a coincidence, not a contract.
///
/// Every in-process caller in the runner bin uses THIS, never the lib function
/// directly.
pub(crate) fn promote_tier_to_account(
) -> anyhow::Result<(qontinui_runner_lib::profiles::TierWrite, std::path::PathBuf)> {
    let outcome = qontinui_runner_lib::profiles::promote_tier_to_account();
    if matches!(
        outcome,
        Ok((qontinui_runner_lib::profiles::TierWrite::Written, _))
    ) {
        invalidate_settings_cache();
    }
    outcome
}

pub fn get_container_settings() -> crate::container::container_config::ContainerConfig {
    let settings = load_settings();
    settings.container
}

pub fn save_container_settings(
    config: crate::container::container_config::ContainerConfig,
) -> Result<(), String> {
    update_settings(|settings| settings.container = config)
}

pub fn get_security_settings() -> crate::security::engine::SecuritySettings {
    let settings = load_settings();
    settings.security
}

pub fn save_security_settings(
    config: crate::security::engine::SecuritySettings,
) -> Result<(), String> {
    update_settings(|settings| settings.security = config)
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

/// Get the default AI launch command template (None = built-in default)
pub fn get_claude_default_launch_command() -> Option<String> {
    crate::config_facade::get_claude_default_launch_command()
}

/// Save the default AI launch command template (None clears the override)
pub fn save_claude_default_launch_command(command: Option<String>) -> Result<(), String> {
    crate::config_facade::save_claude_default_launch_command(command)
}

/// Get the current AI settings
pub fn get_ai_settings() -> AiSettings {
    crate::config_facade::get_setting::<AiSettings>()
}

/// Get the current CI-node settings (plan
/// `2026-07-15-runner-as-ci-node-migration`). The heartbeat, budget publish,
/// and `ci_node` executor all read it at the moment they need it, so a write
/// through [`save_ci_node_settings`] is visible to the next reader without a
/// restart.
pub fn get_ci_node_settings() -> CiNodeSettings {
    load_settings().ci_node
}

/// Persist CI-node settings.
///
/// THE ONLY writer for `ci_node`, and deliberately built on [`update_settings`]
/// rather than a bespoke one: that path starts from the RAW on-disk document
/// (never the env-overlaid view), refuses to write at all when the existing
/// file could not be read or parsed, writes atomically, and drops the parse
/// cache afterwards. Hand-rolling a second writer here would have reintroduced
/// exactly the bug that comment describes — a secondary runner persisting its
/// in-memory overlays over the primary's document as a side effect of an
/// unrelated save.
///
/// Callers: the qontinui-web configuration directive
/// (`ci_node::settings_directive::apply`). The values are validated BEFORE they
/// reach this function — this is a writer, not a gate.
pub fn save_ci_node_settings(ci_node: CiNodeSettings) -> Result<(), String> {
    update_settings(|settings| settings.ci_node = ci_node)
}

/// Save AI settings.
///
/// The whole-`AiSettings` per-instance save stays as-is (its embedded
/// `claude_cli.config_dir` / `account_selection_mode` copies become stale
/// shadows — harmless, because the machine-global overlay in `load_settings`
/// wins unconditionally). ADDITIONALLY mirrors those two roster fields into
/// the machine-global `claude-accounts.json` so mode/pin changes made on any
/// instance reach every instance.
pub fn save_ai_settings(ai_settings: AiSettings) -> Result<(), String> {
    let config_dir = ai_settings.claude_cli.config_dir.clone();
    let selection_mode = ai_settings.claude_cli.account_selection_mode;
    crate::config_facade::save_setting(ai_settings)?;
    crate::claude_accounts::update(move |roster| {
        roster.config_dir = config_dir;
        roster.account_selection_mode = selection_mode;
    })
}

/// Get the World State Verifier settings.
pub fn get_world_state_verifier_settings() -> WorldStateVerifierSettings {
    crate::config_facade::get_setting::<WorldStateVerifierSettings>()
}

/// Save World State Verifier settings.
pub fn save_world_state_verifier_settings(wsv: WorldStateVerifierSettings) -> Result<(), String> {
    crate::config_facade::save_setting(wsv)
}

// ----------------------------------------------------------------------------
// Performance settings — process-cached (plan 2026-07-28 ... Phase 8)
// ----------------------------------------------------------------------------

/// A cached [`PerformanceSettings`] plus the `settings.json` mtime it was
/// built at (`None` = the file did not exist, i.e. a fresh install).
#[derive(Debug, Clone)]
struct CachedPerformance {
    perf: PerformanceSettings,
    mtime: Option<std::time::SystemTime>,
    /// Trust this entry regardless of the file's mtime. Set only by
    /// [`set_performance_cache`], whose whole purpose is to override disk.
    pinned: bool,
}

/// Process-wide snapshot of [`PerformanceSettings`], validated by mtime.
///
/// These values are read on the terminal-spawn path (scrollback capacity,
/// `share_output`), which is exactly the path the same plan is trying to make
/// O(1) — so they must not add another `settings.json` read + double parse per
/// spawn (root cause B5). A cached hit costs one `fs::metadata` instead, the
/// same shape `terminal/transcript.rs` already uses for its file cache.
///
/// The mtime check is not just tidiness. `settings.json` is shared by every
/// runner instance on the box, so a save from a temp runner (or a hand edit)
/// must not leave the primary — which policy forbids restarting — serving a
/// stale `share_terminal_output`. That knob is a privacy declaration; silently
/// ignoring a change to it would be worse than the file read it saves.
///
/// The cell is initialised to `None` with **no work in the initialiser**: the
/// fill calls [`load_settings_full`], which itself can read the keychain and
/// even write a migrated settings file, and a `OnceLock` initialiser that
/// re-entered `get_performance_settings` would deadlock the process with no
/// escape.
static PERFORMANCE_CACHE: std::sync::OnceLock<std::sync::RwLock<Option<CachedPerformance>>> =
    std::sync::OnceLock::new();

fn performance_cache() -> &'static std::sync::RwLock<Option<CachedPerformance>> {
    PERFORMANCE_CACHE.get_or_init(|| std::sync::RwLock::new(None))
}

/// `settings.json`'s last-modified time, or `None` when it cannot be stated
/// (missing file, unresolvable path). `None` is treated as "no evidence the
/// cache is still valid", which forces a re-read rather than trusting it.
fn settings_mtime() -> Option<std::time::SystemTime> {
    let path = resolve_settings_path().ok()?;
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Get the performance caps.
///
/// Served from the process cache while `settings.json`'s mtime matches what
/// the cache was built at; otherwise re-read from disk.
///
/// **A non-authoritative read is never cached.** `load_settings_full` returns
/// `Settings::default()` with `provenance = Unreadable` when the file exists
/// but cannot be read or parsed — e.g. a concurrent atomic rename by another
/// runner instance during our boot. Memoizing that would silently and
/// permanently revert `share_terminal_output` to `true` for the life of a
/// process that policy forbids restarting. So the defaults are returned for
/// this call only, and the next call retries; the surrounding code takes the
/// same stance (`read_settings_from_disk` refuses to synthesize defaults;
/// `update_settings` refuses to write on a non-authoritative base).
pub fn get_performance_settings() -> PerformanceSettings {
    let mtime = settings_mtime();
    {
        let cell = performance_cache();
        let guard = match cell.read() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(cached) = guard.as_ref() {
            if cached.pinned || cached.mtime == mtime {
                return cached.perf.clone();
            }
        }
    }

    let loaded = load_settings_full();
    let perf = loaded.settings.performance.clone();
    if loaded.is_authoritative() {
        store_performance_cache(CachedPerformance {
            perf: perf.clone(),
            mtime,
            pinned: false,
        });
    } else {
        error!(
            "performance settings: settings.json unreadable ({}) — using built-in caps for this \
             call WITHOUT caching them; will retry on the next read",
            loaded.error.as_deref().unwrap_or("unknown error")
        );
    }
    perf
}

/// Persist performance caps and refresh the process cache so the change is
/// live for the next terminal spawn.
pub fn save_performance_settings(perf: PerformanceSettings) -> Result<(), String> {
    crate::config_facade::save_setting(perf.clone())?;
    // Re-stamp with the mtime our own write just produced, so this entry is a
    // normal (invalidatable) cache hit rather than a pin — a peer runner that
    // writes the file a second later must still win.
    store_performance_cache(CachedPerformance {
        perf,
        mtime: settings_mtime(),
        pinned: false,
    });
    Ok(())
}

/// Pin the cached snapshot to `perf` regardless of what is on disk.
///
/// The seam tests use to exercise a non-default cap without touching the real
/// settings file. **Test-only by construction**: pinned entries survive an
/// mtime change, which would defeat the cross-instance invalidation production
/// depends on — [`save_performance_settings`] re-stamps the real mtime instead.
#[cfg(test)]
pub fn set_performance_cache(perf: PerformanceSettings) {
    store_performance_cache(CachedPerformance {
        perf,
        mtime: None,
        pinned: true,
    });
}

/// Serializes tests that mutate the process-global performance cache.
///
/// `cargo test` runs tests in parallel threads inside one process, so two
/// tests pinning different caps would race. Every test that calls
/// [`set_performance_cache`] holds this for its duration — including tests in other modules
/// (`terminal::session`, `commands::terminal`), which is why it is `pub`.
#[cfg(test)]
static PERF_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquire the performance-cache test lock. Poison is ignored: a panicking
/// test must not wedge every other test that touches the cache.
#[cfg(test)]
pub fn perf_test_lock() -> std::sync::MutexGuard<'static, ()> {
    match PERF_TEST_LOCK.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn store_performance_cache(entry: CachedPerformance) {
    let cell = performance_cache();
    let mut guard = match cell.write() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(entry);
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

// ============================================================================
// Cost Budget Settings
// ============================================================================

/// The per-run AI cost cap, **sanitized** — i.e. the budget a run will really
/// enforce, not the raw bytes on disk.
///
/// Routed through [`crate::config_facade::get_setting`] rather than a direct
/// `load_settings()` field read so this getter and every other settings
/// section share one load path; the `SettingsField` impl for `TokenBudget`
/// existed with no caller until this function.
pub fn get_cost_budget_settings() -> crate::cost_management::budget::TokenBudget {
    crate::config_facade::get_setting::<crate::cost_management::budget::TokenBudget>().sanitized()
}

/// Persist a new per-run AI cost cap.
///
/// Stores exactly what it is handed. The **caller** decides whether to
/// sanitize first — [`crate::commands::cost_budget_settings`] does, so that a
/// load-then-save round trip through the UI is a fixed point rather than a
/// silent `phase_budgets` deletion; see that module for the reasoning.
/// [`crate::cost_management::budget::TokenBudget::from_settings`] sanitizes
/// again at use, which is what keeps a hand-edited `settings.json` safe.
pub fn save_cost_budget_settings(
    budget: crate::cost_management::budget::TokenBudget,
) -> Result<(), String> {
    crate::config_facade::save_setting(budget)
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
    update_settings(|settings| settings.saved_projects = projects)
}

/// Get the `SavedProject.id` of the active project, if one is selected.
pub fn get_active_project_id() -> Option<String> {
    load_settings()
        .active_project_id
        .filter(|id| !id.trim().is_empty())
}

/// Set (or clear, with `None`) the active project.
pub fn save_active_project_id(id: Option<String>) -> Result<(), String> {
    let normalized = id.filter(|s| !s.trim().is_empty());
    update_settings(|settings| settings.active_project_id = normalized)
}

// ============================================================================
// Helper Task Queue accessors
// ============================================================================

/// Get the current Helper Task Queue settings.
pub fn get_helper_tasks_settings() -> HelperTasksSettings {
    crate::config_facade::get_setting::<HelperTasksSettings>()
}

/// Save Helper Task Queue settings.
pub fn save_helper_tasks_settings(helper_tasks: HelperTasksSettings) -> Result<(), String> {
    crate::config_facade::save_setting(helper_tasks)
}

// ============================================================================
// Cloud Session Sync accessors
// ============================================================================

/// Get the cloud session sync consent flag (gate 1). Default false.
pub fn get_cloud_sync_enabled() -> bool {
    load_settings().cloud_sync_enabled
}

/// Persist the cloud session sync consent flag.
pub fn save_cloud_sync_enabled(enabled: bool) -> Result<(), String> {
    update_settings(|settings| settings.cloud_sync_enabled = enabled)
}

/// Get the cloud memory link-expansion arm flag. Default false — see
/// [`Settings::memory_link_expansion_enabled`].
pub fn get_memory_link_expansion_enabled() -> bool {
    load_settings().memory_link_expansion_enabled
}

/// Get the session metadata sync consent flag (gate 2). Default true.
pub fn get_session_metadata_sync_enabled() -> bool {
    load_settings().session_metadata_sync_enabled
}

/// Persist the session metadata sync consent flag.
pub fn save_session_metadata_sync_enabled(enabled: bool) -> Result<(), String> {
    update_settings(|settings| settings.session_metadata_sync_enabled = enabled)
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
    update_settings(|settings| settings.lock_yield_policy = policy)
}

// ============================================================================
// Session Guard accessors (plan
// 2026-08-07-runner-resource-guard-and-session-protection, Part B item 2)
// ============================================================================

/// Get the live-session protection floors.
///
/// Read at the moment a decision needs them (the spawn gate, the Settings
/// panel), like [`get_ci_node_settings`] — so a save through
/// [`save_session_guard_settings`] is live for the *next* spawn with no
/// restart. Deliberately NOT process-cached the way
/// [`get_performance_settings`] is: this is one settings read per new terminal,
/// not per emitted output chunk, and a stale floor is a guard that protects the
/// machine the operator used to have.
pub fn get_session_guard_settings() -> SessionGuardSettings {
    load_settings().session_guard
}

/// Persist the live-session protection floors.
///
/// Built on [`update_settings`] for the same reason
/// [`save_ci_node_settings`] is: that path starts from the RAW on-disk
/// document rather than the env-overlaid view, refuses to write at all when the
/// existing file could not be read or parsed, writes atomically, and drops the
/// parse cache afterwards. A secondary runner must never persist its in-memory
/// overlays over the primary's document as a side effect of saving a floor.
pub fn save_session_guard_settings(session_guard: SessionGuardSettings) -> Result<(), String> {
    update_settings(|settings| settings.session_guard = session_guard)
}

#[cfg(test)]
mod openai_compatible_defaults_tests {
    use super::*;

    // ── settings.json parse cache (Phase 6, B5) ─────────────────────────────

    /// Both spawn paths read + DOUBLE-parsed `settings.json` on every open. The
    /// cache must serve an unchanged file, and — this is the part that would
    /// silently break the app — it must NOT serve a file that has changed.
    #[test]
    fn settings_cache_serves_unchanged_and_invalidates_on_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, br#"{"claude_default_launch_command":"first"}"#).expect("write");

        let first = read_settings_from_path(&path);
        assert_eq!(first.provenance, SettingsProvenance::Loaded);
        assert_eq!(
            first.settings.claude_default_launch_command.as_deref(),
            Some("first")
        );

        // Unchanged file → cache hit, same values.
        let cached = read_settings_from_path(&path);
        assert_eq!(
            cached.settings.claude_default_launch_command.as_deref(),
            Some("first")
        );

        // Rewrite with DIFFERENT content. The length differs, so the guard
        // catches it even if the filesystem timestamp has not ticked.
        std::fs::write(
            &path,
            br#"{"claude_default_launch_command":"second-and-longer"}"#,
        )
        .expect("rewrite");
        let refreshed = read_settings_from_path(&path);
        assert_eq!(
            refreshed.settings.claude_default_launch_command.as_deref(),
            Some("second-and-longer"),
            "an mtime/size change must invalidate the cached parse"
        );
    }

    // ── shared tier writer × this module's reader ───────────────────────────

    /// The tier WRITE lives in the lib (`profiles::promote_tier_to_account_at`)
    /// because `settings` is in the runner BIN's module tree and the headless
    /// pair door is a second bin. This test pins the contract across that
    /// boundary in both directions:
    ///
    /// * the `settings.json` the lib CREATES on a fresh headless box — two keys
    ///   and nothing else — must deserialize into a full [`Settings`], every
    ///   other field coming from its serde default; and
    /// * the tier it wrote must read back as [`RunnerTier::QontinuiAccount`]
    ///   with `tier_initialized` set, so `migrate_tier_in_place` does not
    ///   re-infer `Local` over it on the next boot.
    ///
    /// Without this, the lib could silently write a document the bin cannot
    /// parse — which `load_settings` reports as NON-authoritative and therefore
    /// as a runner with no tier at all.
    #[test]
    fn minimal_promoted_settings_json_parses() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        let outcome = qontinui_runner_lib::profiles::promote_tier_to_account_at(&path, false)
            .expect("the lib writer must create an absent settings.json");
        assert_eq!(outcome, qontinui_runner_lib::profiles::TierWrite::Written);

        let loaded = read_settings_from_path(&path);
        assert_eq!(
            loaded.provenance,
            SettingsProvenance::Loaded,
            "the lib-created settings.json must parse as an AUTHORITATIVE load"
        );
        assert_eq!(loaded.settings.tier, RunnerTier::QontinuiAccount);
        assert!(
            loaded.settings.tier_initialized,
            "tier_initialized must be set, or migrate_tier_in_place re-infers Local over it"
        );

        // And the one-shot inference is now a no-op on that document.
        let mut s = loaded.settings.clone();
        assert!(
            !migrate_tier_in_place(
                &mut s, /* server_mode = */ false, /* paired = */ false,
                /* disk_runner_token = */ false,
            )
            .changed(),
            "a promoted document must need no migration"
        );
        assert_eq!(s.tier, RunnerTier::QontinuiAccount);
    }

    /// A corrupt file must never be cached: the "settings unreadable" banner
    /// has to clear the moment the operator fixes the file, and a missing file
    /// must keep reporting `FreshInstall` rather than a stale prior parse.
    #[test]
    fn settings_cache_never_serves_a_missing_or_corrupt_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        assert_eq!(
            read_settings_from_path(&path).provenance,
            SettingsProvenance::FreshInstall
        );

        std::fs::write(&path, br#"{"claude_default_launch_command":"good"}"#).expect("write");
        assert_eq!(
            read_settings_from_path(&path).provenance,
            SettingsProvenance::Loaded
        );

        std::fs::write(&path, b"{ this is not json").expect("corrupt");
        assert_eq!(
            read_settings_from_path(&path).provenance,
            SettingsProvenance::Unreadable,
            "a corrupt file must not be masked by the previous good parse"
        );

        std::fs::write(&path, br#"{"claude_default_launch_command":"good"}"#).expect("repair");
        let repaired = read_settings_from_path(&path);
        assert_eq!(repaired.provenance, SettingsProvenance::Loaded);
        assert_eq!(
            repaired.settings.claude_default_launch_command.as_deref(),
            Some("good")
        );
    }

    /// **F5 regression.** Resolving the config dir CREATES NOTHING.
    ///
    /// `get_config_dir` `create_dir_all`s, and `config_report`'s layer 2 used
    /// to call it — so a typo'd `QONTINUI_CONFIG_DIR` was brought into
    /// existence by the very report asked to explain why the runner could not
    /// find its settings, and then printed as though the machine had always
    /// been configured that way. The pure `resolve_config_dir_from` exists so
    /// this is assertable against a path the test owns, with no
    /// `set_var("QONTINUI_CONFIG_DIR")` racing every sibling test.
    #[test]
    fn resolve_config_dir_creates_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let typo = tmp.path().join("qonitnui-typo");
        assert!(!typo.exists(), "fixture precondition");

        let (dir, source) = resolve_config_dir_from(Some(typo.to_string_lossy().to_string()), None)
            .expect("an env override always resolves");
        assert_eq!(dir, typo);
        assert_eq!(source.as_str(), "env:QONTINUI_CONFIG_DIR");
        assert!(
            !typo.exists(),
            "resolving must not materialize the directory it names"
        );

        // The platform arm, same property, and the `com.qontinui.runner` join
        // is asserted as a LITERAL because it is the on-disk contract.
        let platform = tmp.path().join("platform-config");
        let (dir, source) = resolve_config_dir_from(None, Some(platform.clone()))
            .expect("a platform dir always resolves");
        assert_eq!(dir, platform.join("com.qontinui.runner"));
        assert_eq!(source.as_str(), "platform_config_dir");
        assert!(!dir.exists(), "the platform arm must not create either");

        // An exported-but-EMPTY override falls through to the platform arm —
        // the documented emptiness filter, pinned here because the split above
        // is where it could have been dropped.
        let (dir, source) =
            resolve_config_dir_from(Some(String::new()), Some(platform.clone())).expect("resolves");
        assert_eq!(dir, platform.join("com.qontinui.runner"));
        assert_eq!(source.as_str(), "platform_config_dir");

        // Neither input available is a genuine failure, never a fallback.
        assert!(resolve_config_dir_from(None, None).is_err());
    }

    /// **F6 regression.** The parse error a corrupt `settings.json` produces
    /// carries NO content from the file.
    ///
    /// `serde_json::Error`'s Display for a DATA error (`invalid type`,
    /// `invalid value`) quotes the offending value — and this file holds
    /// `web_integration.runner_token` and `qontinui_user_id`. That string
    /// reaches both the user-facing `unreadable_message()` banner and
    /// `config_report`'s layer 1, so it is bounded at the source to category +
    /// position.
    #[test]
    fn settings_parse_error_carries_no_content_from_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");

        // A type mismatch on a field, with a secret-shaped value in the slot.
        // This is the shape whose Display quotes the value back.
        // Split so the source carries no contiguous high-entropy literal next to a
        // credential keyword — gitleaks' `generic-api-key` fires on that shape.
        // `concat!` is compile-time, so the value and type are unchanged.
        let secret = concat!("AbCdEf0123456789", "AbCdEf0123456789xyz");
        std::fs::write(
            &path,
            format!(r#"{{"restate":{{"ingress_port":"{secret}"}}}}"#),
        )
        .expect("write");

        let loaded = read_settings_from_path(&path);
        assert_eq!(loaded.provenance, SettingsProvenance::Unreadable);
        // The user-facing banner is built from the same string, so checking it
        // covers both consumers at once.
        let banner = loaded.unreadable_message();
        let error = loaded
            .error
            .clone()
            .expect("an unreadable load records its error");
        assert!(
            !error.contains(secret),
            "the parse error quoted the file's own value: {error}"
        );
        assert!(
            error.starts_with("parse failed: JSON ") && error.contains(" error at line "),
            "the error must be category + position: {error}"
        );
        assert!(!banner.contains(secret), "the banner leaked it: {banner}");
    }

    /// Fresh install: no `openai_compatible` key at all → serde `default`
    /// fires and DeepSeek is configured out of the box (plan D3).
    #[test]
    fn missing_block_gets_deepseek_defaults() {
        let parsed: OpenAiCompatibleSettings =
            serde_json::from_str("{}").expect("empty object must deserialize");
        assert_eq!(parsed.base_url, "https://api.deepseek.com");
        assert_eq!(parsed.model, "deepseek-chat");
    }

    /// REGRESSION: every runner install that saved AI settings before this
    /// field had a meaningful default persisted `"base_url": ""` /
    /// `"model": ""`. Serde `default` does NOT fire for a present-but-empty
    /// value, so without the empty-is-unset normalization those installs get
    /// an empty base_url and the provider dies with "base_url is empty" —
    /// D3's DeepSeek default would reach ONLY fresh installs. Caught by the
    /// Phase 4 manual E2E on a real box, not by unit tests that construct
    /// settings directly.
    #[test]
    fn persisted_empty_strings_get_deepseek_defaults() {
        let on_disk = r#"{"base_url":"","model":"","timeout_seconds":600}"#;
        let parsed: OpenAiCompatibleSettings =
            serde_json::from_str(on_disk).expect("legacy settings must deserialize");
        assert_eq!(parsed.base_url, "https://api.deepseek.com");
        assert_eq!(parsed.model, "deepseek-chat");
        assert_eq!(parsed.timeout_seconds, 600);
    }

    #[test]
    fn whitespace_and_null_are_also_unset() {
        let parsed: OpenAiCompatibleSettings =
            serde_json::from_str(r#"{"base_url":"   ","model":null}"#).expect("must deserialize");
        assert_eq!(parsed.base_url, "https://api.deepseek.com");
        assert_eq!(parsed.model, "deepseek-chat");
    }

    /// A real operator-configured endpoint must survive untouched — the
    /// normalization only replaces empties, it never overrides a value.
    #[test]
    fn configured_endpoint_is_preserved() {
        let parsed: OpenAiCompatibleSettings =
            serde_json::from_str(r#"{"base_url":"http://localhost:8080/v1","model":"llama-3"}"#)
                .expect("must deserialize");
        assert_eq!(parsed.base_url, "http://localhost:8080/v1");
        assert_eq!(parsed.model, "llama-3");
    }
}

#[cfg(test)]
mod performance_settings_tests {
    use super::*;

    /// Every default reproduces the constant that was hardcoded before
    /// Phase 8. If one of these changes, an existing install's behavior
    /// changes on upgrade — which is exactly what the phase promised not to
    /// do.
    #[test]
    fn defaults_match_the_previously_hardcoded_values() {
        let p = PerformanceSettings::default();
        assert_eq!(p.max_webgl_panes, 8);
        assert_eq!(p.background_flush_interval_ms, 250);
        assert_eq!(
            p.unwatched_flush_interval_ms, 0,
            "0 means 'no webview emit', the tier's defined behavior"
        );
        assert_eq!(p.scrollback_capacity_bytes, 1_048_576);
        assert_eq!(p.grid_scan_interval_ms, 1500);
        assert!(p.share_terminal_output, "was a hardcoded `true`");
        assert_eq!(
            p.redact_terminal_secrets, None,
            "None keeps redaction following share_output, as before"
        );
        assert_eq!(p.max_sessions_warn, 30);
    }

    /// A settings.json written before this key existed must load with every
    /// default — the back-compat guarantee for the whole phase.
    #[test]
    fn missing_block_loads_defaults() {
        let parsed: PerformanceSettings =
            serde_json::from_str("{}").expect("empty object must deserialize");
        assert_eq!(parsed, PerformanceSettings::default());
    }

    /// The same, one level up: a whole `Settings` with no `performance` key.
    #[test]
    fn settings_without_the_performance_key_loads_defaults() {
        let parsed: Settings =
            serde_json::from_str(r#"{"app_mode":"advanced"}"#).expect("must deserialize");
        assert_eq!(parsed.performance, PerformanceSettings::default());
    }

    /// A partially-specified block keeps the operator's value and defaults
    /// the rest — so adding a knob later never rewrites an existing one.
    #[test]
    fn partial_block_defaults_the_rest() {
        let parsed: PerformanceSettings = serde_json::from_str(r#"{"max_sessions_warn": 12}"#)
            .expect("partial object must deserialize");
        assert_eq!(parsed.max_sessions_warn, 12);
        assert_eq!(parsed.max_webgl_panes, 8);
        assert_eq!(parsed.scrollback_capacity_bytes, 1_048_576);
    }

    /// Serialize → deserialize is lossless, including the tri-state
    /// `redact_terminal_secrets`.
    #[test]
    fn round_trips_through_json() {
        let original = PerformanceSettings {
            max_webgl_panes: 4,
            background_flush_interval_ms: 500,
            unwatched_flush_interval_ms: 2000,
            scrollback_capacity_bytes: 4_194_304,
            grid_scan_interval_ms: 3000,
            share_terminal_output: false,
            redact_terminal_secrets: Some(true),
            max_sessions_warn: 50,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let back: PerformanceSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, original);
    }

    /// The grid-scan floor is applied at USE, not at save — an operator's
    /// stored value round-trips unmangled while the scanner still never
    /// spins faster than the floor.
    #[test]
    fn grid_scan_floor_applies_at_use() {
        let p = PerformanceSettings {
            grid_scan_interval_ms: 5,
            ..PerformanceSettings::default()
        };
        assert_eq!(p.grid_scan_interval_ms, 5, "stored value is untouched");
        assert_eq!(
            p.effective_grid_scan_interval_ms(),
            MIN_GRID_SCAN_INTERVAL_MS
        );
        let stock = PerformanceSettings::default();
        assert_eq!(stock.effective_grid_scan_interval_ms(), 1500);
    }

    /// Same for the scrollback ring: a value below the 64 KiB floor is
    /// clamped at use, and the stock 1 MiB passes through untouched.
    #[test]
    fn scrollback_floor_applies_at_use() {
        let p = PerformanceSettings {
            scrollback_capacity_bytes: 128,
            ..PerformanceSettings::default()
        };
        assert_eq!(p.effective_scrollback_capacity(), MIN_SCROLLBACK_CAPACITY);
        let stock = PerformanceSettings::default();
        assert_eq!(stock.effective_scrollback_capacity(), 1_048_576);
    }

    /// The `background` knob reaches the emission path as the operator's own
    /// value — no floor, no rounding. The regression this pins is the one the
    /// merge-train plan's D3 found: the field existed, was persisted and was
    /// documented as consumed, while the tier used a compiled-in 250 ms.
    #[test]
    fn background_flush_interval_is_the_stored_value() {
        use std::time::Duration;
        assert_eq!(
            PerformanceSettings::default().background_flush_interval(),
            crate::terminal::visibility::BACKGROUND_FLUSH_INTERVAL,
            "the stock value must still be the constant the tier used before"
        );
        let p = PerformanceSettings {
            background_flush_interval_ms: 1000,
            ..PerformanceSettings::default()
        };
        assert_eq!(p.background_flush_interval(), Duration::from_millis(1000));
        let none = PerformanceSettings {
            background_flush_interval_ms: 0,
            ..PerformanceSettings::default()
        };
        assert_eq!(
            none.background_flush_interval(),
            Duration::ZERO,
            "0 is 'no coalescing', not a floor to be clamped away"
        );
    }

    /// `unwatched_flush_interval_ms` is tri-state-ish: `0` is the tier's OFF
    /// switch (and the stock default), anything positive is a cadence. A
    /// `Duration::ZERO` leaking out here would turn every stock install's
    /// silent tier into an uncoalesced emitter.
    #[test]
    fn unwatched_flush_interval_zero_means_no_webview_emit() {
        use std::time::Duration;
        assert_eq!(
            PerformanceSettings::default().unwatched_flush_interval(),
            None,
            "stock behavior: the unwatched tier emits nothing to the webview"
        );
        let p = PerformanceSettings {
            unwatched_flush_interval_ms: 2000,
            ..PerformanceSettings::default()
        };
        assert_eq!(
            p.unwatched_flush_interval(),
            Some(Duration::from_millis(2000))
        );
    }

    /// The pin seam serves what it was given, so the tests that exercise a
    /// non-default cap without touching the real settings file are exercising
    /// the same accessor production reads.
    #[test]
    fn pinned_cache_is_what_the_accessor_serves() {
        let _guard = perf_test_lock();
        let custom = PerformanceSettings {
            max_sessions_warn: 3,
            scrollback_capacity_bytes: 2_000_000,
            share_terminal_output: false,
            ..PerformanceSettings::default()
        };
        set_performance_cache(custom.clone());
        assert_eq!(get_performance_settings(), custom);

        set_performance_cache(PerformanceSettings::default());
        assert_eq!(get_performance_settings(), PerformanceSettings::default());
    }

    /// Pinning is what makes the other modules' cache tests deterministic:
    /// a pin survives whatever `settings.json` says, so a test never depends
    /// on the developer's real file. (The disk-read arm — including the
    /// never-cache-a-non-authoritative-read rule — is deliberately NOT unit
    /// tested: exercising it means letting `load_settings_full` touch the
    /// operator's real settings dir, which it may WRITE to for the
    /// tier/local_user_id migration.)
    #[test]
    fn pinning_is_independent_of_disk() {
        let _guard = perf_test_lock();
        let a = PerformanceSettings {
            max_sessions_warn: 1,
            ..PerformanceSettings::default()
        };
        let b = PerformanceSettings {
            max_sessions_warn: 2,
            ..PerformanceSettings::default()
        };
        set_performance_cache(a);
        assert_eq!(get_performance_settings().max_sessions_warn, 1);
        set_performance_cache(b);
        assert_eq!(get_performance_settings().max_sessions_warn, 2);

        set_performance_cache(PerformanceSettings::default());
    }

    /// The soft session rail is a NUMBER the UI compares against — there is
    /// no "enabled" flag, no refusal path, and nothing here that a spawn
    /// could consult to say no. Locked in as a test because §5 of the plan
    /// explicitly rejects a hard cap.
    #[test]
    fn session_rail_is_advisory_only() {
        let p = PerformanceSettings::default();
        // A number, always positive, and the type carries no refusal state.
        assert!(p.max_sessions_warn > 0);
        // Setting it to zero does not become "allow zero sessions" — it is
        // a display threshold, so it simply warns from the first session.
        let eager = PerformanceSettings {
            max_sessions_warn: 0,
            ..p
        };
        assert_eq!(eager.max_sessions_warn, 0);
    }
}

#[cfg(test)]
mod account_selection_mode_tests {
    use super::AccountSelectionMode;

    /// The bare-string spelling on the account feed's wire MUST equal the one
    /// serde produces for the same enum. If someone renames a variant and
    /// updates only one of the two, this reddens instead of the coord half
    /// silently seeing an unknown mode.
    #[test]
    fn selection_mode_str_matches_serde() {
        for mode in [
            AccountSelectionMode::Manual,
            AccountSelectionMode::LeastUsage,
        ] {
            let serde_spelling = serde_json::to_value(mode).expect("serializes");
            assert_eq!(serde_spelling, serde_json::json!(mode.as_str()));
        }
        // Pin the literals too: the coord half codes against these strings.
        assert_eq!(AccountSelectionMode::Manual.as_str(), "manual");
        assert_eq!(AccountSelectionMode::LeastUsage.as_str(), "least_usage");
    }
}

#[cfg(test)]
mod load_persist_tests {
    use super::*;

    /// Every process-env input `load_settings_full` reads on the path this
    /// module's tests exercise. Captured (and restored) as one set so a test
    /// cannot leak a value — or a removal — into a sibling.
    const LOAD_ENV_KEYS: &[&str] = &[
        "QONTINUI_CONFIG_DIR",
        "QONTINUI_SECURE_STORAGE_DIR",
        "QONTINUI_INSTANCE_NAME",
        "QONTINUI_SERVER_MODE",
        "QONTINUI_RUNNER_TIER",
        "QONTINUI_WEB_BACKEND_URL",
        "QONTINUI_RUNNER_TOKEN",
        "QONTINUI_RESTATE_INGRESS_PORT",
        "QONTINUI_DISABLE_KEYCHAIN",
        // `claude_accounts` resolves the machine-global roster from
        // `dirs::config_dir()`, deliberately ignoring `QONTINUI_CONFIG_DIR`.
        // On Linux that honours `XDG_CONFIG_HOME`, which is what makes the
        // roster half of this test hermetic there. (On Windows `dirs` asks the
        // known-folder API instead, so the roster read falls back to the real
        // machine — the assertion below still holds, it just observes the real
        // roster rather than the fixture's.)
        "XDG_CONFIG_HOME",
    ];

    /// A load-time persist must be built from the RAW ON-DISK document, so
    /// none of the in-memory-only env overlays can reach `settings.json`.
    ///
    /// # The defect this pins
    ///
    /// `apply_web_integration_env_overlay` copies `QONTINUI_RUNNER_TOKEN` — a
    /// documented runtime-only override for headless deploys — into
    /// `settings.web_integration.runner_token` BEFORE the tier migration reads
    /// that field. The migration saw a token, called the promotion durable, and
    /// `save_settings` (which serializes the WHOLE struct) wrote the promoted
    /// tier AND the env-supplied credential into the operator's file. Two
    /// consequences: the `QONTINUI_RUNNER_TIER=local` escape hatch was honoured
    /// in memory and lost on disk — permanently, because
    /// `tier_is_open_to_inference` then closes on `qontinui_account` — and a
    /// process-scoped credential outlived its process.
    ///
    /// The fixture is that exact launch: a box latched at `local` with an empty
    /// token on disk, started with the headless web-integration pair plus the
    /// opt-out. It leaves `local_user_id` empty so the load persists for the
    /// OTHER reason (the lazy UUID mint) — the side door, which is the whole
    /// point: suppressing `needs_persist` alone never closed this.
    ///
    /// Asserted against the FILE BYTES, not the returned struct. The struct is
    /// SUPPOSED to carry the overlays; the bug was that the file did too.
    #[test]
    fn env_overlays_never_reach_the_persisted_settings_document() {
        let _g = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(LOAD_ENV_KEYS);

        let config_dir = tempfile::tempdir().expect("tempdir");
        let xdg_dir = tempfile::tempdir().expect("tempdir");
        let path = config_dir.path().join(SETTINGS_FILE);

        // The document on disk: initialized at `local`, NO runner token, web
        // integration off, no local_user_id yet.
        let on_disk_json = serde_json::json!({
            "tier": "local",
            "tier_initialized": true,
            "local_user_id": "",
            "claude_config_dirs": [],
            "web_integration": {
                "enabled": false,
                "backend_url": "https://disk.example",
                "runner_token": "",
            },
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&on_disk_json).unwrap()).expect("write");

        // A machine-global Claude roster that the overlay WILL apply in memory
        // (on the platforms where `dirs::config_dir()` follows the env).
        let roster_dir = xdg_dir.path().join("com.qontinui.runner");
        std::fs::create_dir_all(&roster_dir).expect("mkdir");
        std::fs::write(
            roster_dir.join("claude-accounts.json"),
            br#"{"claude_config_dirs":["/roster/marker"]}"#,
        )
        .expect("write roster");

        std::env::set_var("QONTINUI_CONFIG_DIR", config_dir.path());
        // Empty dir ⇒ no `paired_user.json` ⇒ not paired, so pairing cannot
        // supply the durable signal this test is about the ABSENCE of.
        std::env::set_var("QONTINUI_SECURE_STORAGE_DIR", config_dir.path());
        std::env::set_var("XDG_CONFIG_HOME", xdg_dir.path());
        // Primary, not a supervisor-launched secondary: the persist guard must
        // be OPEN, or this test would pass for the wrong reason.
        std::env::remove_var("QONTINUI_INSTANCE_NAME");
        std::env::remove_var("QONTINUI_SERVER_MODE");
        // The Tier-2 post-upgrade probe at the end of `load_settings_full`
        // reads the credential store; keep it off the OS keychain.
        std::env::set_var("QONTINUI_DISABLE_KEYCHAIN", "1");
        // The headless launch, verbatim.
        std::env::set_var("QONTINUI_WEB_BACKEND_URL", "https://env-only.example");
        std::env::set_var("QONTINUI_RUNNER_TOKEN", "env-only-runner-token");
        std::env::set_var("QONTINUI_RESTATE_INGRESS_PORT", "19999");
        std::env::set_var("QONTINUI_RUNNER_TIER", "local");

        let loaded = load_settings_full();
        assert_eq!(
            loaded.provenance,
            SettingsProvenance::Loaded,
            "fixture must load authoritatively, or nothing would persist"
        );

        // In memory the overlays ARE in force — that is what they are for.
        assert_eq!(
            loaded.settings.web_integration.runner_token, "env-only-runner-token",
            "the runtime-only override must still apply for this process"
        );
        assert_eq!(
            loaded.settings.web_integration.backend_url,
            "https://env-only.example"
        );
        assert!(
            loaded.settings.web_integration.enabled,
            "the env pair enables it"
        );
        assert_eq!(loaded.settings.restate.ingress_port, 19999);
        assert_eq!(
            loaded.settings.tier,
            RunnerTier::Local,
            "QONTINUI_RUNNER_TIER=local is the documented opt-out"
        );

        // …and NONE of it reached the file.
        let bytes = std::fs::read(&path).expect("settings.json must still exist");
        let text = String::from_utf8(bytes).expect("utf-8");
        let written: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");

        assert!(
            !written["local_user_id"].as_str().unwrap_or("").is_empty(),
            "the load must actually have PERSISTED (the lazy local_user_id mint) — \
             otherwise every assertion below passes vacuously. Got: {text}"
        );
        assert_eq!(
            written["tier"], "local",
            "the escape hatch must survive on disk, not only in memory"
        );
        assert_eq!(written["tier_initialized"], true);
        assert_eq!(
            written["web_integration"]["runner_token"], "",
            "QONTINUI_RUNNER_TOKEN is a RUNTIME-ONLY override — persisting it \
             makes a process-scoped credential outlive its process"
        );
        assert_eq!(
            written["web_integration"]["backend_url"],
            "https://disk.example"
        );
        assert_eq!(written["web_integration"]["enabled"], false);
        assert_ne!(
            written["restate"]["ingress_port"], 19999,
            "the supervisor's per-process Restate ports are in-memory only"
        );
        assert_eq!(
            written["claude_config_dirs"],
            serde_json::json!([]),
            "the machine-global Claude roster is an overlay; `claude-accounts.json` \
             wins on every load, so the per-instance copy must not be written"
        );
        assert!(
            !text.contains("env-only-runner-token") && !text.contains("env-only.example"),
            "no env-supplied web-integration value may appear anywhere in the \
             file: {text}"
        );
    }
}
