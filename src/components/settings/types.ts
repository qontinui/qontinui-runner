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
