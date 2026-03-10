/**
 * Page Catalog
 *
 * Provides a catalog of all known pages in both the runner and web apps,
 * derived from the shared navigation package. Used by the Specs page to
 * identify pages that don't have specs yet.
 */

import { getAllItems, getItemGroup } from "qontinui-navigation";

// ============================================================================
// Types
// ============================================================================

export interface KnownPage {
  /** Navigation item ID */
  navId: string;
  /** Expected spec ID for matching */
  expectedSpecId: string;
  /** Display label */
  label: string;
  /** App name */
  appName: "Qontinui Runner" | "Qontinui Web";
  /** Navigation group label (e.g., "RUN", "BUILD") */
  group: string;
  /** Whether this page is hidden in production */
  hiddenInProd: boolean;
  /** Page description */
  description: string;
}

// ============================================================================
// Navigation ID → Spec ID Mappings
// ============================================================================

/**
 * Special mappings where the runner spec ID doesn't follow the default
 * `runner:{navId}` convention.
 */
const RUNNER_SPEC_ID_OVERRIDES: Record<string, string> = {
  "workflow-queue": "runner:execute",
  "unified-workflow-builder": "runner:workflows",
  "run-recap": "runner:run-summary",
  "run-image": "runner:run-image-recognition",
  tasks: "runner:scheduled-tasks",
};

/**
 * Get the expected spec ID for a navigation item on a given platform.
 */
function getExpectedSpecId(navId: string, platform: "runner" | "web"): string {
  if (platform === "runner") {
    return RUNNER_SPEC_ID_OVERRIDES[navId] ?? `runner:${navId}`;
  }
  // Web pages use the nav ID directly as the spec ID
  return navId;
}

// ============================================================================
// Page Catalog
// ============================================================================

/**
 * Returns all known pages across both apps, derived from the navigation
 * package. Parent containers (items with `hasChildren: true`) are excluded
 * since they are not standalone pages.
 */
export function getKnownPages(): KnownPage[] {
  const allItems = getAllItems();
  const pages: KnownPage[] = [];

  for (const item of allItems) {
    // Skip parent containers — they are not pages themselves
    if (item.hasChildren) {
      continue;
    }

    const group = getItemGroup(item.id);
    const groupLabel = group?.label ?? "UNKNOWN";

    const isRunnerAvailable = !item.platforms || item.platforms.includes("runner");
    const isWebAvailable = !item.platforms || item.platforms.includes("web");

    if (isRunnerAvailable) {
      pages.push({
        navId: item.id,
        expectedSpecId: getExpectedSpecId(item.id, "runner"),
        label: item.label,
        appName: "Qontinui Runner",
        group: groupLabel,
        hiddenInProd: item.hiddenInProd ?? false,
        description: item.description ?? "",
      });
    }

    if (isWebAvailable) {
      pages.push({
        navId: item.id,
        expectedSpecId: getExpectedSpecId(item.id, "web"),
        label: item.label,
        appName: "Qontinui Web",
        group: groupLabel,
        hiddenInProd: item.hiddenInProd ?? false,
        description: item.description ?? "",
      });
    }
  }

  return pages;
}
