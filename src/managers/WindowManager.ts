/**
 * WindowManager
 *
 * Manages window minimization and restoration during workflow execution.
 * Tracks whether the window was auto-minimized to determine if it should be restored.
 */

import { getCurrentWindow } from "@tauri-apps/api/window";
import { createLogger } from "@/lib/logger";

const logger = createLogger("WindowManager");

class WindowManager {
  private wasAutoMinimized = false;

  /**
   * Minimize the window and mark it as auto-minimized
   */
  async minimize(): Promise<void> {
    try {
      const window = getCurrentWindow();
      await window.minimize();
      this.wasAutoMinimized = true;
      logger.debug("Window minimized");
    } catch (error) {
      console.error("[WINDOW] Failed to minimize:", error);
    }
  }

  /**
   * Restore the window if it was auto-minimized
   */
  async restoreIfMinimized(): Promise<void> {
    if (!this.wasAutoMinimized) {
      logger.debug("Window was not auto-minimized, skipping restore");
      return;
    }

    try {
      const window = getCurrentWindow();
      await window.unminimize();
      await window.setFocus();
      this.wasAutoMinimized = false;
      logger.debug("Window restored and focused");
    } catch (error) {
      console.error("[WINDOW] Failed to restore window:", error);
    }
  }

  /**
   * Check if window was auto-minimized
   */
  isAutoMinimized(): boolean {
    return this.wasAutoMinimized;
  }

  /**
   * Reset auto-minimize state (useful for cleanup)
   */
  reset(): void {
    this.wasAutoMinimized = false;
  }
}

// Export singleton instance
export const windowManager = new WindowManager();
