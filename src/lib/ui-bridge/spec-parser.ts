/**
 * spec-parser.ts
 *
 * Pure spec parsing functions used by the specs page and inspector.
 *
 * Converts raw SDK discover responses into typed DiscoveredSpec objects.
 */

import type {
  DiscoveredSpec,
  SpecGroup,
  SpecAssertion,
  SpecConfig,
} from "@/lib/spec-prompt-builder";

// =============================================================================
// Individual parsers
// =============================================================================

/** Parse a raw assertion object into a typed SpecAssertion, or null if invalid. */
export function parseAssertion(raw: Record<string, unknown>): SpecAssertion | null {
  if (typeof raw.id !== "string" || typeof raw.description !== "string") return null;
  return {
    id: raw.id,
    description: raw.description,
    category: typeof raw.category === "string" ? raw.category : "unknown",
    severity:
      raw.severity === "critical" || raw.severity === "warning" || raw.severity === "info"
        ? raw.severity
        : "info",
    enabled: raw.enabled !== false,
    target: raw.target as Record<string, unknown> | undefined,
    assertionType: typeof raw.assertionType === "string" ? raw.assertionType : undefined,
    condition: raw.condition as Record<string, unknown> | undefined,
  };
}

/** Parse a raw group object into a typed SpecGroup, or null if invalid. */
export function parseGroup(raw: Record<string, unknown>): SpecGroup | null {
  if (typeof raw.id !== "string" || typeof raw.name !== "string") return null;
  const rawAssertions = Array.isArray(raw.assertions) ? raw.assertions : [];
  const assertions = rawAssertions
    .map((a: unknown) => parseAssertion(a as Record<string, unknown>))
    .filter((a): a is SpecAssertion => a !== null);
  return {
    id: raw.id,
    name: raw.name,
    description: typeof raw.description === "string" ? raw.description : "",
    category: typeof raw.category === "string" ? raw.category : "unknown",
    assertions,
    source: typeof raw.source === "string" ? raw.source : undefined,
  };
}

/** Parse a raw specs array into typed DiscoveredSpec[]. */
export function parseDiscoveredSpecs(rawSpecs: unknown): DiscoveredSpec[] {
  if (!Array.isArray(rawSpecs)) return [];
  const results: DiscoveredSpec[] = [];
  for (const raw of rawSpecs) {
    if (typeof raw !== "object" || raw === null) continue;
    const obj = raw as Record<string, unknown>;
    if (typeof obj.specId !== "string") continue;
    const rawConfig = obj.config as Record<string, unknown> | undefined;
    if (!rawConfig) continue;
    const rawGroups = Array.isArray(rawConfig.groups) ? rawConfig.groups : [];
    const groups = rawGroups
      .map((g: unknown) => parseGroup(g as Record<string, unknown>))
      .filter((g): g is SpecGroup => g !== null);
    const config: SpecConfig = {
      version: typeof rawConfig.version === "string" ? rawConfig.version : "1.0.0",
      description: typeof rawConfig.description === "string" ? rawConfig.description : "",
      groups,
      metadata: rawConfig.metadata as SpecConfig["metadata"],
    };
    results.push({ specId: obj.specId, config });
  }
  return results;
}

// =============================================================================
// Response unwrapping
// =============================================================================

/**
 * Unwrap an SDK proxy response envelope and extract the raw specs array.
 * Handles multiple response shapes:
 *   - res.specs (direct)
 *   - res.data.specs (wrapped)
 *   - snapshot specStore fallback (object keyed by specId)
 */
export function unwrapSpecResponse(raw: unknown): unknown[] {
  if (!raw || typeof raw !== "object") return [];
  const obj = raw as Record<string, unknown>;

  // Direct: { specs: [...] }
  if (Array.isArray(obj.specs)) return obj.specs;

  // Wrapped: { data: { specs: [...] } }
  const inner = obj.data;
  if (inner && typeof inner === "object" && !Array.isArray(inner)) {
    const innerObj = inner as Record<string, unknown>;
    if (Array.isArray(innerObj.specs)) return innerObj.specs;
  }

  // Snapshot specStore fallback: { specStore: { specId: config, ... } }
  const specStore = obj.specStore || obj.specs;
  if (specStore && typeof specStore === "object" && !Array.isArray(specStore)) {
    return Object.entries(specStore).map(([specId, config]) => ({
      specId,
      config,
    }));
  }

  return [];
}
