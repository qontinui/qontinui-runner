/**
 * Configuration Utilities
 *
 * Shared utility functions for configuration management.
 */

import type { ConfigMetadata, Config, LoadConfigurationData } from "./types";

/**
 * Derive projectId from metadata name if not explicitly set.
 * This matches the Rust project_id() method behavior.
 */
export function deriveProjectId(metadata?: ConfigMetadata): string | undefined {
  if (metadata?.projectId) {
    return metadata.projectId;
  }
  if (metadata?.name) {
    // Sanitize name for use as projectId (same as Rust's project_id() method)
    return metadata.name.replace(/[^a-zA-Z0-9_-]/g, "_").toLowerCase();
  }
  return undefined;
}

/**
 * Extract filename without extension from a path
 */
export function extractConfigName(configPath: string): string {
  const pathParts = configPath.split(/[/\\]/);
  const fullFilename = pathParts[pathParts.length - 1] || "config.json";
  return fullFilename.replace(/\.[^/.]+$/, "");
}

/**
 * Create a Config object from loaded configuration data
 */
export function createConfigFromData(
  data: LoadConfigurationData | undefined,
  configPath: string,
): Config {
  const projectId = deriveProjectId(data?.metadata);
  const nameWithoutExtension = extractConfigName(configPath);

  return {
    name: data?.metadata?.name || nameWithoutExtension,
    version: "1.0.0",
    statesCount: data?.states?.length || 0,
    workflowsCount: data?.workflows?.length || 0,
    workflows: data?.workflows || [],
    images: data?.images || [],
    states: data?.states || [],
    path: configPath,
    projectId,
  };
}
