/**
 * Type definitions for Mobile Development Feedback
 *
 * These types mirror the Rust types in:
 * - src-tauri/src/commands/mobile.rs
 * - src-tauri/src/database/mod.rs (MobileState, MobileLog)
 */

// =============================================================================
// Mobile Device Types
// =============================================================================

/**
 * Information about a connected Android device
 */
export interface MobileDevice {
  device_id: string;
  device_type: "emulator" | "physical";
  model?: string;
  state: string; // "device", "offline", "unauthorized"
}

// =============================================================================
// Mobile State Types
// =============================================================================

/**
 * Mobile state capture from the database.
 * Stores device/app state at a point in time.
 */
export interface MobileState {
  id: number;
  task_run_id: string;
  timestamp: string;

  // Device info
  device_id?: string;
  device_type?: string;
  device_model?: string;

  // App state
  app_package?: string;
  app_activity?: string;
  app_state?: string;

  // Metro/Expo state
  metro_connected: boolean;
  bundle_status?: string;
  last_reload_type?: string;
  last_reload_time?: string;

  // Capture paths
  screenshot_path?: string;
  logcat_path?: string;

  // Error summary
  has_errors: boolean;
  error_summary?: string;

  created_at: string;
}

// =============================================================================
// Mobile Log Types
// =============================================================================

/**
 * Log source types for mobile logs.
 */
export type MobileLogSource = "logcat" | "metro" | "build" | "device";

/**
 * Mobile log entry from the database.
 * Stores parsed log entries from Metro, Logcat, etc.
 */
export interface MobileLog {
  id: number;
  task_run_id: string;
  mobile_state_id?: number;

  // Log classification
  log_source: string;
  log_level?: string;
  log_tag?: string;

  // Content
  message: string;
  raw_line?: string;
  data?: string;

  // Error details
  error_type?: string;
  error_code?: string;
  stack_trace?: string;
  file_path?: string;
  line_number?: number;
  column_number?: number;

  // Timing
  timestamp: string;
  device_timestamp?: string;

  created_at: string;
}

// =============================================================================
// Screenshot Result Types
// =============================================================================

/**
 * Result of capturing a mobile screenshot.
 */
export interface MobileScreenshotResult {
  success: boolean;
  screenshot_path?: string;
  device_id: string;
  error?: string;
}

/**
 * Result of capturing logcat.
 */
export interface LogcatResult {
  success: boolean;
  log_path?: string;
  lines_captured: number;
  error?: string;
}

/**
 * Result of capturing mobile feedback (screenshot + logcat).
 */
export interface MobileFeedbackResult {
  mobile_state_id: number;
  screenshot_path?: string;
  logcat_path?: string;
  device_id: string;
  has_errors: boolean;
}

// =============================================================================
// Command Response Types
// =============================================================================

/**
 * Standard command response from Tauri commands.
 */
export interface CommandResponse<T = unknown> {
  success: boolean;
  message?: string;
  data?: T;
}
