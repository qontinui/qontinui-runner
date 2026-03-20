/**
 * ConfigManager
 *
 * Singleton manager for storing current configuration state.
 * Used by EventHandlers to get the current config's projectId for test run reporting.
 *
 * The projectId is persisted to localStorage to survive HMR and module reloads.
 * This ensures the config's projectId is always available when execution starts.
 */

import { instanceStorage } from "@/lib/instance-storage";
import { createLogger } from "@/lib/logger";

const log = createLogger("ConfigManager");

// Key for storing the config's projectId in localStorage
// This is SEPARATE from "qontinui-selected-project" which stores the runner's UI selection
const CONFIG_PROJECT_ID_KEY = "qontinui-config-projectId";

class ConfigManagerClass {
  private static instanceCount = 0;
  private instanceId: number;
  private currentProjectId: string | null = null;

  constructor() {
    ConfigManagerClass.instanceCount++;
    this.instanceId = ConfigManagerClass.instanceCount;
    log.debug(`Instance #${this.instanceId} created`);

    // Restore projectId from localStorage if this is a new instance (e.g., after HMR)
    const stored = instanceStorage.getItem(CONFIG_PROJECT_ID_KEY);
    if (stored) {
      this.currentProjectId = stored;
      log.debug(`Instance #${this.instanceId} Restored projectId from localStorage:`, stored);
    }
  }

  /**
   * Set the current project ID from loaded config metadata.
   * Called when a configuration is loaded.
   * Also persists to localStorage to survive module reloads.
   */
  setProjectId(projectId: string | null): void {
    log.debug(`Instance #${this.instanceId} setProjectId called with:`, projectId);
    this.currentProjectId = projectId;

    // Persist to localStorage for robustness against HMR/module reloads
    if (projectId) {
      instanceStorage.setItem(CONFIG_PROJECT_ID_KEY, projectId);
      log.debug(`Instance #${this.instanceId} Project ID set and persisted:`, projectId);
    } else {
      instanceStorage.removeItem(CONFIG_PROJECT_ID_KEY);
      log.debug(`Instance #${this.instanceId} Project ID cleared`);
    }
  }

  /**
   * Get the current project ID.
   * Priority:
   * 1. In-memory currentProjectId (from loaded config)
   * 2. localStorage CONFIG_PROJECT_ID_KEY (persisted config projectId)
   * 3. localStorage "qontinui-selected-project" (runner's UI selection, only if config has no projectId)
   */
  getProjectId(): string | null {
    // 1. Check in-memory value first
    if (this.currentProjectId) {
      log.debug(
        `Instance #${this.instanceId} getProjectId returning in-memory projectId:`,
        this.currentProjectId,
      );
      return this.currentProjectId;
    }

    // 2. Check localStorage for persisted config projectId
    const configProjectId = instanceStorage.getItem(CONFIG_PROJECT_ID_KEY);
    if (configProjectId) {
      log.debug(
        `Instance #${this.instanceId} getProjectId returning localStorage config projectId:`,
        configProjectId,
      );
      // Restore to in-memory for future calls
      this.currentProjectId = configProjectId;
      return configProjectId;
    }

    // 3. Fall back to runner's UI selection (only for configs without explicit projectId)
    log.debug(
      `Instance #${this.instanceId} getProjectId: no config projectId, checking selected-project fallback`,
    );
    const parsed = instanceStorage.getJSON<{ selectedProjectId?: string } | null>(
      "qontinui-selected-project",
      null,
    );
    if (parsed) {
      const fallbackId = parsed.selectedProjectId || null;
      log.debug(
        `Instance #${this.instanceId} getProjectId returning selected-project fallback:`,
        fallbackId,
      );
      return fallbackId;
    }
    log.debug(`Instance #${this.instanceId} getProjectId returning null (no projectId found)`);
    return null;
  }

  /**
   * Clear the current project ID.
   * Called when configuration is unloaded.
   */
  clear(): void {
    this.currentProjectId = null;
    instanceStorage.removeItem(CONFIG_PROJECT_ID_KEY);
    log.debug(`Instance #${this.instanceId} State cleared`);
  }
}

export const configManager = new ConfigManagerClass();
