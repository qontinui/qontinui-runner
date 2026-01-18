/**
 * Mobile Service
 *
 * Provides Tauri invoke calls for mobile development feedback.
 * Wraps the mobile Tauri commands from src-tauri/src/commands/mobile.rs
 */

import { invoke } from "@tauri-apps/api/core";
import type {
  CommandResponse,
  MobileDevice,
  MobileState,
  MobileLog,
  MobileScreenshotResult,
  LogcatResult,
  MobileFeedbackResult,
} from "../types/mobile";

/**
 * Service for accessing mobile development feedback via Tauri commands.
 */
export const mobileService = {
  // ===========================================================================
  // Device Management
  // ===========================================================================

  /**
   * List all connected Android devices and emulators.
   */
  async listDevices(): Promise<CommandResponse<MobileDevice[]>> {
    return invoke("list_mobile_devices");
  },

  // ===========================================================================
  // Screenshot Capture
  // ===========================================================================

  /**
   * Capture a screenshot from a connected Android device.
   * @param taskRunId - Optional task run ID to associate the screenshot with
   * @param deviceId - Optional device ID (uses first connected device if not specified)
   * @param outputDir - Optional output directory
   */
  async captureScreenshot(
    taskRunId?: string,
    deviceId?: string,
    outputDir?: string,
  ): Promise<CommandResponse<MobileScreenshotResult>> {
    return invoke("capture_mobile_screenshot", {
      taskRunId,
      deviceId,
      outputDir,
    });
  },

  // ===========================================================================
  // Logcat Capture
  // ===========================================================================

  /**
   * Capture logcat output from a connected Android device.
   * @param taskRunId - Optional task run ID to associate logs with
   * @param deviceId - Optional device ID
   * @param lines - Number of lines to capture (default: 500)
   * @param filterApp - If true, filter to React Native logs only
   * @param outputDir - Optional output directory
   */
  async captureLogcat(
    taskRunId?: string,
    deviceId?: string,
    lines?: number,
    filterApp?: boolean,
    outputDir?: string,
  ): Promise<CommandResponse<LogcatResult>> {
    return invoke("capture_mobile_logcat", {
      taskRunId,
      deviceId,
      lines,
      filterApp,
      outputDir,
    });
  },

  // ===========================================================================
  // Combined Capture
  // ===========================================================================

  /**
   * Capture both screenshot and logcat from a mobile device.
   * @param taskRunId - Task run ID to associate captures with
   * @param deviceId - Optional device ID
   * @param logcatLines - Number of logcat lines to capture (default: 500)
   */
  async captureFeedback(
    taskRunId: string,
    deviceId?: string,
    logcatLines?: number,
  ): Promise<CommandResponse<MobileFeedbackResult>> {
    return invoke("capture_mobile_feedback", {
      taskRunId,
      deviceId,
      logcatLines,
    });
  },

  // ===========================================================================
  // Mobile State CRUD
  // ===========================================================================

  /**
   * Get mobile state captures for a task run.
   * @param taskRunId - Task run ID to query
   * @param limit - Maximum number of results (default: 100)
   */
  async getMobileStates(
    taskRunId: string,
    limit?: number,
  ): Promise<CommandResponse<MobileState[]>> {
    return invoke("get_mobile_states", { taskRunId, limit });
  },

  /**
   * Get the latest mobile state for a task run.
   * @param taskRunId - Task run ID to query
   */
  async getLatestMobileState(taskRunId: string): Promise<CommandResponse<MobileState | null>> {
    return invoke("get_latest_mobile_state", { taskRunId });
  },

  /**
   * Create a new mobile state capture.
   */
  async createMobileState(input: {
    task_run_id: string;
    device_id?: string;
    device_type?: string;
    device_model?: string;
    app_package?: string;
    app_activity?: string;
    app_state?: string;
    metro_connected?: boolean;
    bundle_status?: string;
    last_reload_type?: string;
    screenshot_path?: string;
    logcat_path?: string;
    has_errors?: boolean;
    error_summary?: string;
  }): Promise<CommandResponse<MobileState>> {
    return invoke("create_mobile_state", { input });
  },

  // ===========================================================================
  // Mobile Log CRUD
  // ===========================================================================

  /**
   * Get mobile logs for a task run.
   * @param taskRunId - Task run ID to query
   * @param logSource - Optional filter by log source (e.g., "logcat", "metro")
   * @param errorsOnly - If true, only return error logs
   * @param limit - Maximum number of results (default: 500)
   */
  async getMobileLogs(
    taskRunId: string,
    logSource?: string,
    errorsOnly?: boolean,
    limit?: number,
  ): Promise<CommandResponse<MobileLog[]>> {
    return invoke("get_mobile_logs", {
      taskRunId,
      logSource,
      errorsOnly,
      limit,
    });
  },

  /**
   * Get mobile error logs for a task run.
   * @param taskRunId - Task run ID to query
   * @param limit - Maximum number of results (default: 100)
   */
  async getMobileErrors(taskRunId: string, limit?: number): Promise<CommandResponse<MobileLog[]>> {
    return invoke("get_mobile_errors", { taskRunId, limit });
  },

  /**
   * Create a new mobile log entry.
   */
  async createMobileLog(input: {
    task_run_id: string;
    mobile_state_id?: number;
    log_source: string;
    log_level?: string;
    log_tag?: string;
    message: string;
    raw_line?: string;
    data?: string;
    error_type?: string;
    error_code?: string;
    stack_trace?: string;
    file_path?: string;
    line_number?: number;
    column_number?: number;
    device_timestamp?: string;
  }): Promise<CommandResponse<MobileLog>> {
    return invoke("create_mobile_log", { input });
  },

  // ===========================================================================
  // Cleanup
  // ===========================================================================

  /**
   * Delete all mobile data for a task run.
   * @param taskRunId - Task run ID to delete data for
   */
  async deleteMobileData(
    taskRunId: string,
  ): Promise<CommandResponse<{ states_deleted: number; logs_deleted: number }>> {
    return invoke("delete_mobile_data", { taskRunId });
  },
};
