// Shared types for settings components

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

export interface Monitor {
  index: number;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
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
export type AiProvider = "claude_cli" | "claude_api";
export type CliExecutionMode = "auto" | "windows_native" | "wsl" | "native";

export interface ClaudeCliSettings {
  execution_mode: CliExecutionMode;
  custom_path?: string;
  timeout_seconds: number;
}

export interface ClaudeApiSettings {
  model: string;
  max_tokens: number;
}

export interface AiSettings {
  provider: AiProvider;
  claude_cli: ClaudeCliSettings;
  claude_api: ClaudeApiSettings;
  /** Default iteration threshold for including video in auto-refine (0 = never) */
  auto_refine_video_after_iterations: number;
}

export interface AiConnectionTestResult {
  success: boolean;
  message: string;
  provider: string;
}
