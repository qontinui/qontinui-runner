/**
 * ConfigManager
 *
 * Singleton manager for storing current configuration state.
 * Used by EventHandlers to get the current config's projectId for test run reporting.
 *
 * The projectId is persisted to localStorage to survive HMR and module reloads.
 * This ensures the config's projectId is always available when execution starts.
 */

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
    console.log(`[CONFIG_MANAGER] Instance #${this.instanceId} created`);

    // Restore projectId from localStorage if this is a new instance (e.g., after HMR)
    const stored = localStorage.getItem(CONFIG_PROJECT_ID_KEY);
    if (stored) {
      this.currentProjectId = stored;
      console.log(
        `[CONFIG_MANAGER #${this.instanceId}] Restored projectId from localStorage:`,
        stored,
      );
    }
  }

  /**
   * Set the current project ID from loaded config metadata.
   * Called when a configuration is loaded.
   * Also persists to localStorage to survive module reloads.
   */
  setProjectId(projectId: string | null): void {
    console.log(`[CONFIG_MANAGER #${this.instanceId}] setProjectId called with:`, projectId);
    this.currentProjectId = projectId;

    // Persist to localStorage for robustness against HMR/module reloads
    if (projectId) {
      localStorage.setItem(CONFIG_PROJECT_ID_KEY, projectId);
      console.log(`[CONFIG_MANAGER #${this.instanceId}] Project ID set and persisted:`, projectId);
    } else {
      localStorage.removeItem(CONFIG_PROJECT_ID_KEY);
      console.log(`[CONFIG_MANAGER #${this.instanceId}] Project ID cleared`);
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
      console.log(
        `[CONFIG_MANAGER #${this.instanceId}] getProjectId returning in-memory projectId:`,
        this.currentProjectId,
      );
      return this.currentProjectId;
    }

    // 2. Check localStorage for persisted config projectId
    const configProjectId = localStorage.getItem(CONFIG_PROJECT_ID_KEY);
    if (configProjectId) {
      console.log(
        `[CONFIG_MANAGER #${this.instanceId}] getProjectId returning localStorage config projectId:`,
        configProjectId,
      );
      // Restore to in-memory for future calls
      this.currentProjectId = configProjectId;
      return configProjectId;
    }

    // 3. Fall back to runner's UI selection (only for configs without explicit projectId)
    console.log(
      `[CONFIG_MANAGER #${this.instanceId}] getProjectId: no config projectId, checking selected-project fallback`,
    );
    const stored = localStorage.getItem("qontinui-selected-project");
    if (stored) {
      try {
        const parsed = JSON.parse(stored);
        const fallbackId = parsed.selectedProjectId || null;
        console.log(
          `[CONFIG_MANAGER #${this.instanceId}] getProjectId returning selected-project fallback:`,
          fallbackId,
        );
        return fallbackId;
      } catch {
        console.log(
          `[CONFIG_MANAGER #${this.instanceId}] getProjectId: selected-project parse failed`,
        );
        return null;
      }
    }
    console.log(
      `[CONFIG_MANAGER #${this.instanceId}] getProjectId returning null (no projectId found)`,
    );
    return null;
  }

  /**
   * Clear the current project ID.
   * Called when configuration is unloaded.
   */
  clear(): void {
    this.currentProjectId = null;
    localStorage.removeItem(CONFIG_PROJECT_ID_KEY);
    console.log(`[CONFIG_MANAGER #${this.instanceId}] State cleared`);
  }
}

export const configManager = new ConfigManagerClass();
