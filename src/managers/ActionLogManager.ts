/**
 * ActionLogManager
 *
 * Manages action log refresh triggers. When tree events are received,
 * this manager notifies all subscribers to refresh their action log data.
 */

import { createLogger } from "@/lib/logger";

const logger = createLogger("ActionLogManager");

type RefreshCallback = () => void;

class ActionLogManager {
  private refreshCallbacks = new Set<RefreshCallback>();

  /**
   * Register a callback to be called when refresh is needed
   * @returns Unsubscribe function
   */
  onRefreshNeeded(callback: RefreshCallback): () => void {
    logger.debug("Refresh callback registered");
    this.refreshCallbacks.add(callback);
    return () => {
      logger.debug("Refresh callback unregistered");
      this.refreshCallbacks.delete(callback);
    };
  }

  /**
   * Trigger refresh for all subscribers
   * Called by event router when tree events are received
   */
  triggerRefresh(): void {
    logger.debug(`triggerRefresh called, notifying ${this.refreshCallbacks.size} callbacks`);
    this.refreshCallbacks.forEach((cb) => {
      try {
        cb();
      } catch (error) {
        console.error("[ACTION_LOG_MGR] Error in refresh callback:", error);
      }
    });
  }

  /**
   * Get number of active subscribers
   */
  getSubscriberCount(): number {
    return this.refreshCallbacks.size;
  }
}

// Export singleton instance
export const actionLogManager = new ActionLogManager();
