/**
 * spec-parser.ts
 *
 * Parses raw spec responses from the UI Bridge SDK into DiscoveredSpec objects.
 */

import type { DiscoveredSpec, SpecConfig } from "@/lib/spec-prompt-builder";

/**
 * Unwrap the raw response from the SDK's discover/getSpecs endpoint.
 * Handles both direct spec arrays and wrapped `{ specs: [...] }` responses.
 */
export function unwrapSpecResponse(data: unknown): unknown[] {
  if (Array.isArray(data)) return data;
  if (data && typeof data === "object") {
    const obj = data as Record<string, unknown>;
    if (Array.isArray(obj.specs)) return obj.specs;
    if (Array.isArray(obj.data)) return obj.data;
  }
  return [];
}

/**
 * Parse raw spec objects into typed DiscoveredSpec instances.
 */
export function parseDiscoveredSpecs(rawSpecs: unknown[]): DiscoveredSpec[] {
  const results: DiscoveredSpec[] = [];

  for (const raw of rawSpecs) {
    if (!raw || typeof raw !== "object") continue;
    const obj = raw as Record<string, unknown>;

    const specId =
      (obj.specId as string) ||
      (obj.spec_id as string) ||
      (obj.id as string) ||
      `spec-${results.length}`;

    const config = (obj.config as SpecConfig) || (obj as unknown as SpecConfig);

    // Validate minimum spec structure
    if (!config || !config.version) continue;
    if (!Array.isArray(config.groups)) {
      config.groups = [];
    }

    results.push({
      specId,
      config,
      appName: (obj.appName as string) || (obj.app_name as string) || undefined,
    });
  }

  return results;
}
