/* eslint-disable no-console */
/**
 * Render Logger Utility
 *
 * Provides a simple API for UI components to log what data they render.
 * This enables Python tests to verify that components display the correct information
 * without needing Playwright or direct DOM access.
 *
 * Usage:
 * ```tsx
 * import { logRender } from '../utils/renderLogger';
 *
 * useEffect(() => {
 *   if (data) {
 *     logRender('MyComponent', 'data_loaded', {
 *       itemCount: data.items.length,
 *       status: data.status,
 *     });
 *   }
 * }, [data]);
 * ```
 */

import { invoke } from "@tauri-apps/api/core";

// Type for component render log entries
export interface ComponentRenderLogEntry {
  id: string;
  timestamp: number;
  component: string;
  trigger: string;
  data: Record<string, unknown>;
  visible_sections?: string[];
  task_run_id?: number;
}

// Wire format for Tauri command (matches Rust struct field names)
interface RenderLogEntryWire {
  id: string;
  timestamp: number;
  component: string;
  trigger: string;
  data: Record<string, unknown>;
  visible_sections?: string[];
  task_run_id?: number;
}

/**
 * Log a component render event.
 *
 * @param component - Name of the component (e.g., "RunRecapTab", "StatusBanner")
 * @param trigger - What triggered the render (e.g., "mount", "data_update", "prop_change")
 * @param data - The data being rendered (component-specific)
 * @param options - Optional additional fields
 */
export async function logRender(
  component: string,
  trigger: string,
  data: Record<string, unknown>,
  options?: {
    visibleSections?: string[];
    taskRunId?: number;
  },
): Promise<void> {
  // Only log in development mode
  if (import.meta.env.PROD) {
    return;
  }

  const entry: RenderLogEntryWire = {
    id: `render-${Date.now()}-${Math.random().toString(36).slice(2, 9)}`,
    timestamp: Date.now(),
    component,
    trigger,
    data,
    visible_sections: options?.visibleSections,
    task_run_id: options?.taskRunId,
  };

  try {
    await invoke("append_render_log", { entry });
  } catch (error) {
    // Silently fail - render logging is non-critical
    console.debug("[RenderLogger] Failed to log render:", error);
  }
}

/**
 * Clear the render log file.
 * Call this at the start of a test to ensure fresh logs.
 */
export async function clearRenderLog(): Promise<void> {
  try {
    await invoke("clear_render_log");
  } catch (error) {
    console.debug("[RenderLogger] Failed to clear render log:", error);
  }
}

/**
 * Load all render log entries.
 * Primarily used by tests to read what was rendered.
 */
export async function loadRenderLog(): Promise<RenderLogEntryWire[]> {
  try {
    const result = await invoke<{
      success: boolean;
      data?: { entries: RenderLogEntryWire[] };
    }>("load_render_log");

    if (result.success && result.data?.entries) {
      return result.data.entries;
    }
    return [];
  } catch (error) {
    console.debug("[RenderLogger] Failed to load render log:", error);
    return [];
  }
}

/**
 * Get the path to the render log file.
 */
export async function getRenderLogPath(): Promise<string | null> {
  try {
    const result = await invoke<{
      success: boolean;
      data?: { path: string };
    }>("get_render_log_path_cmd");

    if (result.success && result.data?.path) {
      return result.data.path;
    }
    return null;
  } catch (error) {
    console.debug("[RenderLogger] Failed to get render log path:", error);
    return null;
  }
}
