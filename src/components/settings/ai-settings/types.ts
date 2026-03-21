import type {
  AiSettings,
  AiProvider,
  CliExecutionMode,
  GeminiAuthMethod,
  AiConnectionTestResult,
  AccountUsageInfo,
  LogFunction,
} from "../types";

export type {
  AiSettings,
  AiProvider,
  CliExecutionMode,
  GeminiAuthMethod,
  AiConnectionTestResult,
  AccountUsageInfo,
  LogFunction,
};

export interface TauriResult<T> {
  success: boolean;
  data?: T;
  message?: string;
}

export interface HasApiKeyData {
  has_key: boolean;
}

export interface ClaudeConfigDirsData {
  dirs: string[];
}

export interface DiscoveredDir {
  path: string;
  label: string;
  source: string;
}

export interface CliAuthStatus {
  has_credentials: boolean;
  expired: boolean;
  is_cli_provider: boolean;
  expires_at: string | null;
  minutes_until_expiry: number | null;
  subscription_type: string | null;
  credentials_path: string | null;
}

export interface ApiKeyState {
  apiKey: string;
  showApiKey: boolean;
  hasApiKey: boolean;
  savingApiKey: boolean;
}

export type ApiKeyAction =
  | { type: "SET_KEY"; value: string }
  | { type: "TOGGLE_SHOW" }
  | { type: "SET_HAS_KEY"; value: boolean }
  | { type: "SET_SAVING"; value: boolean }
  | { type: "KEY_SAVED" };

export function apiKeyReducer(state: ApiKeyState, action: ApiKeyAction): ApiKeyState {
  switch (action.type) {
    case "SET_KEY":
      return { ...state, apiKey: action.value };
    case "TOGGLE_SHOW":
      return { ...state, showApiKey: !state.showApiKey };
    case "SET_HAS_KEY":
      return { ...state, hasApiKey: action.value };
    case "SET_SAVING":
      return { ...state, savingApiKey: action.value };
    case "KEY_SAVED":
      return { ...state, hasApiKey: true, apiKey: "", savingApiKey: false };
    default:
      return state;
  }
}

export const INITIAL_API_KEY_STATE: ApiKeyState = {
  apiKey: "",
  showApiKey: false,
  hasApiKey: false,
  savingApiKey: false,
};

export const DEFAULT_AI_SETTINGS: AiSettings = {
  provider: "claude_cli",
  claude_cli: {
    execution_mode: "auto",
    timeout_seconds: 600,
    config_dir: undefined,
    account_selection_mode: "manual",
  },
  claude_api: {
    model: "claude-sonnet-4-20250514",
    max_tokens: 4096,
  },
  gemini_cli: {
    execution_mode: "auto",
    timeout_seconds: 600,
    auth_method: "oauth",
    model: "gemini-3-flash-preview",
  },
  gemini_api: {
    model: "gemini-3-flash-preview",
    max_output_tokens: 8192,
    temperature: 0.7,
  },
  auto_refine_video_after_iterations: 3,
  interactive_sessions_enabled: true,
};

export const PROVIDER_OPTIONS: {
  value: AiProvider;
  label: string;
  description: string;
}[] = [
  {
    value: "claude_cli",
    label: "Claude Code CLI (Recommended)",
    description: "Uses your Claude Code subscription - no per-token cost",
  },
  {
    value: "claude_api",
    label: "Claude API",
    description: "Direct API access with per-token billing",
  },
  {
    value: "gemini_cli",
    label: "Gemini CLI",
    description: "Google's Gemini CLI with OAuth or API key - free tier available",
  },
  {
    value: "gemini_api",
    label: "Gemini API",
    description: "Direct Gemini API access with per-token billing",
  },
];

export const EXECUTION_MODE_OPTIONS: {
  value: CliExecutionMode;
  label: string;
  description: string;
}[] = [
  {
    value: "auto",
    label: "Auto-detect (Recommended)",
    description: "Automatically detects the best execution method for your platform",
  },
  {
    value: "windows_native",
    label: "Windows Native",
    description: "Run claude.exe directly on Windows",
  },
  {
    value: "wsl",
    label: "WSL (Windows Subsystem for Linux)",
    description: "Run claude via WSL on Windows",
  },
  {
    value: "native",
    label: "Native Unix",
    description: "Native execution on macOS or Linux",
  },
];

export const MODEL_OPTIONS = [
  { value: "claude-sonnet-4-20250514", label: "Claude Sonnet 4 (Recommended)" },
  { value: "claude-opus-4-20250514", label: "Claude Opus 4" },
  { value: "claude-3-5-sonnet-20241022", label: "Claude 3.5 Sonnet" },
  { value: "claude-3-opus-20240229", label: "Claude 3 Opus" },
];

export const GEMINI_MODEL_OPTIONS = [
  { value: "gemini-3-flash-preview", label: "Gemini 3 Flash (Fast/Cheap)" },
  { value: "gemini-3-pro-preview", label: "Gemini 3 Pro" },
  { value: "gemini-2.5-flash", label: "Gemini 2.5 Flash" },
  { value: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
  { value: "gemini-2.0-flash", label: "Gemini 2.0 Flash" },
];

export const GEMINI_AUTH_OPTIONS: {
  value: GeminiAuthMethod;
  label: string;
  description: string;
}[] = [
  {
    value: "oauth",
    label: "OAuth (Google Account)",
    description: "Login with your Google account - 60 req/min, 1000 req/day free",
  },
  {
    value: "api_key",
    label: "API Key",
    description: "Use a Gemini API key - 100 req/day free tier",
  },
];
