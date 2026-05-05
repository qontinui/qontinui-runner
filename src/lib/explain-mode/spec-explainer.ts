/**
 * Spec-enhanced explanation builder.
 * Looks up element descriptions from the UI Bridge spec store to enrich
 * the AI-generated explanations with context about what UI elements do.
 */

import type { SpecConfig } from "@qontinui/ui-bridge/specs";

interface SpecStore {
  getAll(): Array<{ specId: string; config: SpecConfig }>;
  get(specId: string): SpecConfig | undefined;
}

function getSpecStore(): SpecStore | null {
  const bridge = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | { specs?: unknown }
    | undefined;
  const specs = bridge?.specs as Partial<SpecStore> | undefined;
  if (!specs || typeof specs.get !== "function" || typeof specs.getAll !== "function") {
    return null;
  }
  return specs as SpecStore;
}

/**
 * Find spec descriptions relevant to a target element or page.
 */
export function findSpecDescription(targetName: string): string | null {
  const store = getSpecStore();
  if (!store) return null;

  const allSpecs = store.getAll();
  for (const { config } of allSpecs) {
    for (const group of config.groups) {
      for (const assertion of group.assertions) {
        const target = assertion.target;
        const label =
          target.label ??
          (target.type === "ctr" ? target.logicalName : undefined) ??
          "";
        if (label.toLowerCase().includes(targetName.toLowerCase())) {
          return assertion.description;
        }
      }
    }
  }
  return null;
}

/**
 * Get a page-level description from its spec.
 */
export function getPageDescription(pageId: string): string | null {
  const store = getSpecStore();
  if (!store) return null;

  const spec = store.get(pageId);
  return spec?.description ?? null;
}
