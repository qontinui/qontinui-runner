/**
 * ConfigManager
 *
 * Singleton manager for storing current configuration state.
 * Used by EventHandlers to get the current config's projectId for test run reporting.
 */

class ConfigManagerClass {
  private currentProjectId: string | null = null;

  /**
   * Set the current project ID from loaded config metadata.
   * Called when a configuration is loaded.
   */
  setProjectId(projectId: string | null): void {
    this.currentProjectId = projectId;
    if (projectId) {
      console.log("[CONFIG_MANAGER] Project ID set:", projectId);
    } else {
      console.log("[CONFIG_MANAGER] Project ID cleared");
    }
  }

  /**
   * Get the current project ID.
   * Returns the projectId from the loaded config's metadata,
   * or falls back to localStorage selection if not in config.
   */
  getProjectId(): string | null {
    // Prefer projectId from loaded config
    if (this.currentProjectId) {
      return this.currentProjectId;
    }

    // Fall back to localStorage selection (runner's own project selection)
    const stored = localStorage.getItem("qontinui-selected-project");
    if (stored) {
      try {
        const parsed = JSON.parse(stored);
        return parsed.selectedProjectId || null;
      } catch {
        return null;
      }
    }
    return null;
  }

  /**
   * Clear the current project ID.
   * Called when configuration is unloaded.
   */
  clear(): void {
    this.currentProjectId = null;
    console.log("[CONFIG_MANAGER] State cleared");
  }
}

export const configManager = new ConfigManagerClass();
