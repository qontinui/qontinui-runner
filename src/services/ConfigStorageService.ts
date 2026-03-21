/**
 * ConfigStorageService
 *
 * HTTP client for the ConfigStorage API on the runner.
 * Provides CRUD operations for stored configurations.
 */

import { getApiBase, tracedFetch } from "@/lib/runner-api";
import { createLogger } from "@/lib/logger";

const logger = createLogger("ConfigStorage");

/**
 * Metadata for a stored configuration (without full config data)
 */
export interface StoredConfigMetadata {
  id: string;
  name: string;
  description: string;
  workflow_count: number;
  state_count: number;
  image_count: number;
  created_at: string;
  modified_at: string;
}

/**
 * Full stored configuration including config data
 */
export interface StoredConfig extends StoredConfigMetadata {
  config: unknown; // Full QontinuiConfig
  source_path: string | null;
}

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

/**
 * Make an API request to the runner
 */
async function apiRequest<T>(endpoint: string, options: RequestInit = {}): Promise<T> {
  const url = `${getApiBase()}${endpoint}`;
  logger.debug(`${options.method || "GET"} ${endpoint}`);

  try {
    const response = await tracedFetch(url, {
      ...options,
      headers: {
        "Content-Type": "application/json",
        ...options.headers,
      },
    });

    const json = (await response.json()) as ApiResponse<T>;

    if (!response.ok || !json.success) {
      const errorMsg = json.error || `HTTP ${response.status}`;
      console.error(`[CONFIG_STORAGE] Error: ${errorMsg}`);
      throw new Error(errorMsg);
    }

    return json.data as T;
  } catch (error) {
    console.error(`[CONFIG_STORAGE] Request failed:`, error);
    throw error;
  }
}

/**
 * Service for managing stored configurations via the runner HTTP API
 */
export const ConfigStorageService = {
  /**
   * List all stored configurations (metadata only)
   */
  async listConfigs(): Promise<StoredConfigMetadata[]> {
    return apiRequest<StoredConfigMetadata[]>("/configs");
  },

  /**
   * Get a stored configuration by ID
   */
  async getConfig(id: string): Promise<StoredConfig> {
    return apiRequest<StoredConfig>(`/configs/${encodeURIComponent(id)}`);
  },

  /**
   * Import a configuration from a file path
   * @returns The ID of the newly stored configuration
   */
  async importConfig(path: string, name: string): Promise<{ id: string }> {
    return apiRequest<{ id: string }>("/configs", {
      method: "POST",
      body: JSON.stringify({ path, name }),
    });
  },

  /**
   * Update a configuration's metadata
   */
  async updateConfig(id: string, name?: string, description?: string): Promise<void> {
    await apiRequest<void>(`/configs/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify({ name, description }),
    });
  },

  /**
   * Delete a stored configuration
   */
  async deleteConfig(id: string): Promise<void> {
    await apiRequest<void>(`/configs/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
  },

  /**
   * Export a configuration to a file
   */
  async exportConfig(id: string, path: string): Promise<void> {
    await apiRequest<void>(`/configs/${encodeURIComponent(id)}/export`, {
      method: "POST",
      body: JSON.stringify({ path }),
    });
  },
};

export default ConfigStorageService;
