// Shared types for settings components

// Re-export Monitor from shared geometry types
export type { Monitor } from "../../types/geometry";

export interface DebugSettings {
  enable_image_debug: boolean;
  top_matches_count: number;
}

export interface AppSettings {
  auto_load_last_config: boolean;
}

export interface ConnectionInfo {
  version: string;
  url: string;
  token: string;
  userId: string;
  /** Project ID as UUID string (e.g., "fb93478d-98bd-4e40-99f4-0f2c08c1fd5a") */
  projectId: string | null;
  createdAt: string;
  backendUrl: string;
}

export interface WebSocketSettings {
  // Connection
  enabled: boolean;
  url: string;
  token: string;
  projectId: string;
  connected: boolean;
  backendUrl: string;
  /** Custom user-defined name for this runner (e.g., "My Laptop") */
  runnerName: string;

  // Cloud permission status (read-only, from API)
  cloudPermissionEnabled: boolean;
  sessionsLimit: number | null;
  sessionsUsed: number;
  sessionsResetAt: string | null;

  // User controls
  sendToCloud: boolean;
  sendLogs: boolean;
  sendScreenshots: boolean;
  sendVideos: boolean;
}

export type ScreenSelectionType =
  | { type: "all" }
  | { type: "primary" }
  | { type: "specific"; indices: number[] };

export interface ScreenshotCaptureSettings {
  enabled: boolean;
  manualClicksEnabled: boolean;
  outputFolder: string;
  baseImageName: string;
  screens: ScreenSelectionType;
  captureTimings: number[];
}

export interface StorageUsage {
  screenshots: number;
  videos: number;
  screenshotCount: number;
  videoCount: number;
}

export interface StoragePaths {
  screenshot_path: string;
  video_path: string;
  max_screenshot_mb: number;
  max_video_mb: number;
  auto_cleanup: boolean;
}

export type LogFunction = (
  level: "info" | "warning" | "error" | "debug" | "success",
  message: string,
) => void;

export interface UpdateInfo {
  available: boolean;
  version?: string;
  current_version?: string;
  notes?: string;
  development?: boolean;
}

export type UpdateStatus = "idle" | "checking" | "downloading" | "installing" | "error";

// AI Settings Types
export type AiProvider = "claude_cli" | "claude_api" | "gemini_cli" | "gemini_api";
export type CliExecutionMode = "auto" | "windows_native" | "wsl" | "native";
export type GeminiAuthMethod = "oauth" | "api_key";
export type AccountSelectionMode = "manual" | "least_usage";

export interface ClaudeCliSettings {
  execution_mode: CliExecutionMode;
  custom_path?: string;
  timeout_seconds: number;
  /** Custom CLAUDE_CONFIG_DIR for multi-account support */
  config_dir?: string;
  /** How to select which account to use when multiple config dirs exist */
  account_selection_mode?: AccountSelectionMode;
}

export interface AccountUsageInfo {
  config_dir: string;
  label: string;
  utilization: number;
  rate_limit_type: string | null;
  resets_at: number | null;
  status: string | null;
  error: string | null;
}

export interface ClaudeApiSettings {
  model: string;
  max_tokens: number;
}

export interface GeminiCliSettings {
  execution_mode: CliExecutionMode;
  custom_path?: string;
  timeout_seconds: number;
  auth_method: GeminiAuthMethod;
  model: string;
}

export interface GeminiApiSettings {
  model: string;
  max_output_tokens: number;
  temperature: number;
}

export interface AiSettings {
  provider: AiProvider;
  claude_cli: ClaudeCliSettings;
  claude_api: ClaudeApiSettings;
  gemini_cli?: GeminiCliSettings;
  gemini_api?: GeminiApiSettings;
  /** Default iteration threshold for including video in auto-refine (0 = never) */
  auto_refine_video_after_iterations: number;
  /** Enable interactive bidirectional CLI sessions (stream-json protocol).
   * When true, sessions use multi-turn interactive mode with message queuing.
   * When false, sessions use one-shot inline mode. */
  interactive_sessions_enabled: boolean;
}

export interface AiConnectionTestResult {
  success: boolean;
  message: string;
  provider: string;
}

// Agentic Settings Types

export interface CompressionSettings {
  enabled: boolean;
  threshold_tokens: number;
  target_tokens: number;
  keep_recent_items: number;
  summarize_batch_size: number;
  tokens_per_char: number;
}

export interface RetrySettings {
  enabled: boolean;
  max_retries: number;
  base_delay_ms: number;
  max_delay_ms: number;
  exponential_base: number;
  jitter: boolean;
  feedback_injection: boolean;
  retryable_errors: string[];
}

export interface RoutingSettings {
  enabled: boolean;
  simple_model: string;
  medium_model: string;
  complex_model: string;
  file_count_thresholds: [number, number];
  prompt_length_thresholds: [number, number];
  complex_keywords: string[];
  simple_keywords: string[];
}

export interface AgenticSettings {
  compression: CompressionSettings;
  retry: RetrySettings;
  routing: RoutingSettings;
}

// Self-Healing Settings Types

/** LLM mode for self-healing operations */
export type SelfHealingLlmMode = "disabled" | "local_ollama" | "remote_api";

/** API provider for remote LLM in self-healing */
export type SelfHealingApiProvider = "open_ai" | "anthropic";

/** Settings for self-healing automation features */
export interface SelfHealingSettings {
  /** Enable action caching to avoid redundant operations */
  action_caching_enabled: boolean;
  /** Cache TTL in seconds (how long cached actions remain valid) */
  cache_ttl_seconds: number;
  /** Enable visual validation of actions */
  visual_validation_enabled: boolean;
  /** LLM mode for self-healing assistance */
  llm_mode: SelfHealingLlmMode;
  /** Ollama model name (used when llm_mode is LocalOllama) */
  ollama_model: string;
  /** API provider (used when llm_mode is RemoteApi) */
  api_provider: SelfHealingApiProvider;
}

// World State Verifier Settings Types

/** Tri-state mode selector for the VLM-based World State Verifier. */
export type WsvMode = "disabled" | "enabled" | "shadow";

/** Settings for the VLM-based World State Verifier (CUA-WSM / SEAgent judge). */
export interface WorldStateVerifierSettings {
  /** Tri-state mode: disabled, enabled (WSM wins), or shadow (log disagreements only). */
  mode: WsvMode;
  /** llama-swap (or compatible) endpoint base URL. */
  endpoint: string;
  /** Model alias or HuggingFace id to request. */
  model: string;
  /** When true, append pre/post screenshot thumbnails to agentic iteration canvas panels. */
  show_screenshot_evidence: boolean;
}

/** Result shape returned by the `test_wsv_connection` Tauri command. */
export interface WsvConnectionTestResult {
  ok: boolean;
  error: string | null;
  models_available: string[];
  latency_ms: number;
}

/** One row from the shadow-mode disagreements calibration log. */
export interface WsvDisagreementRow {
  id: number;
  task_run_id: string;
  iteration: number;
  text_status: string;
  wsm_status: string;
  text_confidence: number;
  wsm_confidence: number;
  intent: string;
  wsm_observations: string;
  created_at: string;
}
