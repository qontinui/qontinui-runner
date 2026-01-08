/**
 * useConfigFiltering
 *
 * Hook for filtering workflows based on automation-enabled categories.
 * Responsibility: Filter and categorize workflows for display.
 */

import { useCallback } from "react";
import type { Workflow, Category, LogFunction } from "./types";

interface FilteringResult {
  automationWorkflows: Workflow[];
  automationEnabledCategories: string[];
  hiddenCount: number;
}

interface UseConfigFilteringOptions {
  onLog?: LogFunction;
}

interface UseConfigFilteringReturn {
  filterWorkflows: (
    allWorkflows: Workflow[],
    categories: Category[] | string[] | undefined,
  ) => FilteringResult;
  logFilteringInfo: (allWorkflows: Workflow[], filteredWorkflows: Workflow[]) => void;
}

/**
 * Extract automation-enabled category names from categories array
 */
function getAutomationEnabledCategories(categories: Category[] | string[] | undefined): string[] {
  console.log("[FILTER] getAutomationEnabledCategories called with:", categories);

  // Default if no categories defined
  if (!Array.isArray(categories) || categories.length === 0) {
    console.log("[FILTER] No categories array, defaulting to ['main']");
    return ["main"];
  }

  // Check if categories are objects (new format) or strings (legacy format)
  if (typeof categories[0] === "object") {
    // New format: Category[] with automationEnabled flag
    const enabled = (categories as Category[])
      .filter((c) => c.automationEnabled)
      .map((c) => c.name.toLowerCase());
    console.log("[FILTER] Object format categories, automation-enabled:", enabled);
    return enabled;
  }

  // Legacy format: string[] - default to only "main" enabled
  console.log("[FILTER] String format (legacy), defaulting to ['main']");
  return ["main"];
}

/**
 * Filter workflows to show only those in automation-enabled categories
 * and exclude internal workflows
 */
function filterAutomationWorkflows(
  allWorkflows: Workflow[],
  automationEnabledCategories: string[],
): Workflow[] {
  return allWorkflows.filter(
    (w) =>
      w.category &&
      automationEnabledCategories.includes(w.category.toLowerCase()) &&
      (!w.visibility || w.visibility !== "internal"),
  );
}

/**
 * Hook to handle workflow filtering based on categories
 */
export function useConfigFiltering(
  options: UseConfigFilteringOptions = {},
): UseConfigFilteringReturn {
  const { onLog } = options;

  /**
   * Filter workflows based on automation-enabled categories
   */
  const filterWorkflows = useCallback(
    (allWorkflows: Workflow[], categories: Category[] | string[] | undefined): FilteringResult => {
      const automationEnabledCategories = getAutomationEnabledCategories(categories);
      const automationWorkflows = filterAutomationWorkflows(
        allWorkflows,
        automationEnabledCategories,
      );
      const hiddenCount = allWorkflows.length - automationWorkflows.length;

      return {
        automationWorkflows,
        automationEnabledCategories,
        hiddenCount,
      };
    },
    [],
  );

  /**
   * Log detailed information about workflow filtering
   */
  const logFilteringInfo = useCallback(
    (allWorkflows: Workflow[], filteredWorkflows: Workflow[]) => {
      if (!onLog || allWorkflows.length === 0) return;

      const categoryCounts: Record<string, number> = {};
      const visibilityCounts: Record<string, number> = {};

      allWorkflows.forEach((w) => {
        const cat = w.category || "No category";
        const vis = w.visibility || "public";
        categoryCounts[cat] = (categoryCounts[cat] || 0) + 1;
        visibilityCounts[vis] = (visibilityCounts[vis] || 0) + 1;
      });

      const categoryInfo = Object.entries(categoryCounts)
        .map(([cat, count]) => `${cat}: ${count}`)
        .join(", ");

      const visibilityInfo = Object.entries(visibilityCounts)
        .map(([vis, count]) => `${vis}: ${count}`)
        .join(", ");

      onLog("debug", `Workflow categories: ${categoryInfo}`);
      onLog("debug", `Workflow visibility: ${visibilityInfo}`);

      const hiddenCount = allWorkflows.length - filteredWorkflows.length;
      if (hiddenCount > 0) {
        onLog(
          "info",
          `Loaded ${filteredWorkflows.length} workflows for automation (${hiddenCount} hidden)`,
        );
      } else {
        onLog("info", `Loaded ${filteredWorkflows.length} workflows`);
      }

      if (filteredWorkflows.length === 0) {
        onLog(
          "warning",
          "No workflows found in automation-enabled categories. Enable automation for categories in qontinui-web.",
        );
      }
    },
    [onLog],
  );

  return {
    filterWorkflows,
    logFilteringInfo,
  };
}
