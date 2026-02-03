/**
 * Element Description Persistence Service
 *
 * Manages persistent storage of element descriptions across sessions.
 * Uses localStorage for persistence, keyed by page URL origin + element ID.
 *
 * Features:
 * - Auto-save on description generation
 * - Load descriptions on component mount
 * - Multi-page support (different URLs have separate descriptions)
 * - Export/import for sharing descriptions
 */

const STORAGE_KEY = "qontinui-element-descriptions";
const MAX_ENTRIES = 1000; // Limit to prevent localStorage overflow

/**
 * Persisted element description data
 */
export interface PersistedElementDescription {
  elementId: string;
  description: string;
  purpose: string;
  aliases: string[];
  suggestedActions: string[];
  source: "heuristic" | "ai";
  generatedAt: number;
  /** How a user would interact with this element (AI-generated) */
  interaction?: string;
  /** Accessibility notes and considerations (AI-generated) */
  accessibilityNotes?: string | null;
}

/**
 * Storage structure: keyed by page origin
 */
interface DescriptionStore {
  version: number;
  pages: {
    [pageKey: string]: {
      url: string;
      title: string;
      lastUpdated: number;
      descriptions: {
        [elementId: string]: PersistedElementDescription;
      };
    };
  };
}

/**
 * Generate a stable key for a page
 * Uses origin + pathname (without query params) for stability
 */
function getPageKey(url: string): string {
  try {
    const parsed = new URL(url);
    // Use origin + pathname for stability
    return `${parsed.origin}${parsed.pathname}`;
  } catch {
    // Fallback for invalid URLs
    return url;
  }
}

/**
 * Load the description store from localStorage
 */
function loadStore(): DescriptionStore {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored) {
      const parsed = JSON.parse(stored) as DescriptionStore;
      // Validate structure
      if (parsed.version && parsed.pages) {
        return parsed;
      }
    }
  } catch (e) {
    console.warn("[ElementDescriptionService] Failed to load store:", e);
  }

  // Return empty store
  return {
    version: 1,
    pages: {},
  };
}

/**
 * Save the description store to localStorage
 */
function saveStore(store: DescriptionStore): void {
  try {
    // Prune old entries if over limit
    const pageKeys = Object.keys(store.pages);
    if (pageKeys.length > MAX_ENTRIES) {
      // Sort by lastUpdated and remove oldest
      const sorted = pageKeys.sort(
        (a, b) => store.pages[a].lastUpdated - store.pages[b].lastUpdated,
      );
      const toRemove = sorted.slice(0, pageKeys.length - MAX_ENTRIES);
      toRemove.forEach((key) => delete store.pages[key]);
    }

    localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
  } catch (e) {
    console.error("[ElementDescriptionService] Failed to save store:", e);
  }
}

/**
 * Element Description Service
 */
export const ElementDescriptionService = {
  /**
   * Load all descriptions for a specific page
   */
  loadDescriptions(pageUrl: string): Map<string, PersistedElementDescription> {
    const store = loadStore();
    const pageKey = getPageKey(pageUrl);
    const pageData = store.pages[pageKey];

    if (!pageData) {
      return new Map();
    }

    const result = new Map<string, PersistedElementDescription>();
    for (const [elementId, desc] of Object.entries(pageData.descriptions)) {
      result.set(elementId, desc);
    }

    console.log(`[ElementDescriptionService] Loaded ${result.size} descriptions for ${pageKey}`);
    return result;
  },

  /**
   * Save a single description
   */
  saveDescription(
    pageUrl: string,
    pageTitle: string,
    description: PersistedElementDescription,
  ): void {
    const store = loadStore();
    const pageKey = getPageKey(pageUrl);

    if (!store.pages[pageKey]) {
      store.pages[pageKey] = {
        url: pageUrl,
        title: pageTitle,
        lastUpdated: Date.now(),
        descriptions: {},
      };
    }

    store.pages[pageKey].descriptions[description.elementId] = description;
    store.pages[pageKey].lastUpdated = Date.now();

    saveStore(store);
    console.log(
      `[ElementDescriptionService] Saved description for ${description.elementId} on ${pageKey}`,
    );
  },

  /**
   * Save multiple descriptions at once
   */
  saveDescriptions(
    pageUrl: string,
    pageTitle: string,
    descriptions: Map<string, PersistedElementDescription>,
  ): void {
    const store = loadStore();
    const pageKey = getPageKey(pageUrl);

    if (!store.pages[pageKey]) {
      store.pages[pageKey] = {
        url: pageUrl,
        title: pageTitle,
        lastUpdated: Date.now(),
        descriptions: {},
      };
    }

    for (const [elementId, desc] of descriptions) {
      store.pages[pageKey].descriptions[elementId] = desc;
    }
    store.pages[pageKey].lastUpdated = Date.now();

    saveStore(store);
    console.log(
      `[ElementDescriptionService] Saved ${descriptions.size} descriptions for ${pageKey}`,
    );
  },

  /**
   * Delete a description
   */
  deleteDescription(pageUrl: string, elementId: string): void {
    const store = loadStore();
    const pageKey = getPageKey(pageUrl);

    if (store.pages[pageKey]?.descriptions[elementId]) {
      delete store.pages[pageKey].descriptions[elementId];
      store.pages[pageKey].lastUpdated = Date.now();
      saveStore(store);
    }
  },

  /**
   * Clear all descriptions for a page
   */
  clearPage(pageUrl: string): void {
    const store = loadStore();
    const pageKey = getPageKey(pageUrl);

    if (store.pages[pageKey]) {
      delete store.pages[pageKey];
      saveStore(store);
      console.log(`[ElementDescriptionService] Cleared descriptions for ${pageKey}`);
    }
  },

  /**
   * Clear all stored descriptions
   */
  clearAll(): void {
    localStorage.removeItem(STORAGE_KEY);
    console.log("[ElementDescriptionService] Cleared all descriptions");
  },

  /**
   * Get statistics about stored descriptions
   */
  getStats(): {
    totalPages: number;
    totalDescriptions: number;
    aiDescriptions: number;
    oldestEntry: number | null;
    newestEntry: number | null;
  } {
    const store = loadStore();
    const pageKeys = Object.keys(store.pages);

    let totalDescriptions = 0;
    let aiDescriptions = 0;
    let oldest: number | null = null;
    let newest: number | null = null;

    for (const pageData of Object.values(store.pages)) {
      for (const desc of Object.values(pageData.descriptions)) {
        totalDescriptions++;
        if (desc.source === "ai") aiDescriptions++;

        if (oldest === null || desc.generatedAt < oldest) {
          oldest = desc.generatedAt;
        }
        if (newest === null || desc.generatedAt > newest) {
          newest = desc.generatedAt;
        }
      }
    }

    return {
      totalPages: pageKeys.length,
      totalDescriptions,
      aiDescriptions,
      oldestEntry: oldest,
      newestEntry: newest,
    };
  },

  /**
   * Export all descriptions as JSON (for backup/sharing)
   */
  exportAll(): string {
    const store = loadStore();
    return JSON.stringify(store, null, 2);
  },

  /**
   * Import descriptions from JSON
   */
  importFromJson(json: string, merge: boolean = true): number {
    try {
      const imported = JSON.parse(json) as DescriptionStore;

      if (!imported.version || !imported.pages) {
        throw new Error("Invalid description store format");
      }

      if (merge) {
        const current = loadStore();
        // Merge imported pages into current
        for (const [pageKey, pageData] of Object.entries(imported.pages)) {
          if (!current.pages[pageKey]) {
            current.pages[pageKey] = pageData;
          } else {
            // Merge descriptions, newer wins
            for (const [elemId, desc] of Object.entries(pageData.descriptions)) {
              const existing = current.pages[pageKey].descriptions[elemId];
              if (!existing || desc.generatedAt > existing.generatedAt) {
                current.pages[pageKey].descriptions[elemId] = desc;
              }
            }
            current.pages[pageKey].lastUpdated = Math.max(
              current.pages[pageKey].lastUpdated,
              pageData.lastUpdated,
            );
          }
        }
        saveStore(current);
        return Object.keys(imported.pages).length;
      } else {
        // Replace entire store
        saveStore(imported);
        return Object.keys(imported.pages).length;
      }
    } catch (e) {
      console.error("[ElementDescriptionService] Failed to import:", e);
      throw e;
    }
  },

  /**
   * Get all aliases from stored descriptions for a page
   * Useful for cross-referencing with natural language search
   */
  getAliasesForPage(pageUrl: string): Map<string, string[]> {
    const descriptions = this.loadDescriptions(pageUrl);
    const result = new Map<string, string[]>();

    for (const [elementId, desc] of descriptions) {
      if (desc.aliases && desc.aliases.length > 0) {
        result.set(elementId, desc.aliases);
      }
    }

    return result;
  },

  /**
   * Search descriptions across all pages
   * Returns element IDs that match the search term
   */
  searchDescriptions(searchTerm: string): Array<{
    pageUrl: string;
    elementId: string;
    description: PersistedElementDescription;
    matchType: "alias" | "description" | "purpose";
  }> {
    const store = loadStore();
    const results: Array<{
      pageUrl: string;
      elementId: string;
      description: PersistedElementDescription;
      matchType: "alias" | "description" | "purpose";
    }> = [];

    const lowerSearch = searchTerm.toLowerCase();

    for (const pageData of Object.values(store.pages)) {
      for (const [elementId, desc] of Object.entries(pageData.descriptions)) {
        // Check aliases
        if (desc.aliases.some((a) => a.toLowerCase().includes(lowerSearch))) {
          results.push({
            pageUrl: pageData.url,
            elementId,
            description: desc,
            matchType: "alias",
          });
          continue;
        }

        // Check description text
        if (desc.description.toLowerCase().includes(lowerSearch)) {
          results.push({
            pageUrl: pageData.url,
            elementId,
            description: desc,
            matchType: "description",
          });
          continue;
        }

        // Check purpose
        if (desc.purpose.toLowerCase().includes(lowerSearch)) {
          results.push({
            pageUrl: pageData.url,
            elementId,
            description: desc,
            matchType: "purpose",
          });
        }
      }
    }

    return results;
  },
};

export default ElementDescriptionService;
