/**
 * Spec-enhanced explanation builder.
 * Looks up element descriptions from the UI Bridge spec store to enrich
 * the AI-generated explanations with context about what UI elements do.
 */

interface SpecStore {
  getAll(): Array<{ specId: string; config: SpecConfig }>;
  get(specId: string): SpecConfig | undefined;
}

interface SpecConfig {
  description?: string;
  groups: SpecGroup[];
}

interface SpecGroup {
  id: string;
  name: string;
  description: string;
  assertions: SpecAssertion[];
}

interface SpecAssertion {
  id: string;
  description: string;
  target: { type: string; elementId?: string; logicalName?: string; label?: string };
}

function getSpecStore(): SpecStore | null {
  const bridge = (window as unknown as Record<string, unknown>).__UI_BRIDGE__ as
    | { specs?: SpecStore }
    | undefined;
  return bridge?.specs ?? null;
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
        const label = assertion.target.label ?? assertion.target.logicalName ?? "";
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
