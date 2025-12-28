/**
 * FindingCategories.ts
 *
 * Service for managing finding categories.
 * Provides built-in categories and supports custom user-defined categories.
 */

import type { FindingCategory, CategoryStore, BuiltInCategoryId } from "../types/findings";

// Storage key for custom categories
const CATEGORY_STORE_KEY = "qontinui_finding_categories";

/**
 * Built-in categories that cannot be deleted
 */
export const BUILT_IN_CATEGORIES: FindingCategory[] = [
  {
    id: "code_bug",
    name: "Code Bug",
    description: "Actual code issues that can be auto-fixed",
    icon: "Bug",
    color: "red",
    isBuiltIn: true,
    defaultActionType: "auto_fix",
    sortOrder: 1,
  },
  {
    id: "todo",
    name: "TODO",
    description: "Tasks that may require user decisions",
    icon: "CheckSquare",
    color: "amber",
    isBuiltIn: true,
    defaultActionType: "needs_user_input",
    sortOrder: 2,
  },
  {
    id: "security",
    name: "Security",
    description: "Security vulnerabilities or concerns",
    icon: "Shield",
    color: "red",
    isBuiltIn: true,
    defaultActionType: "auto_fix",
    sortOrder: 3,
  },
  {
    id: "config_issue",
    name: "Configuration Issue",
    description: "Configuration or environment problems",
    icon: "Settings",
    color: "orange",
    isBuiltIn: true,
    defaultActionType: "manual",
    sortOrder: 4,
  },
  {
    id: "already_fixed",
    name: "Already Fixed",
    description: "Issues resolved in previous sessions",
    icon: "CheckCircle",
    color: "green",
    isBuiltIn: true,
    defaultActionType: "informational",
    sortOrder: 5,
  },
  {
    id: "expected_behavior",
    name: "Expected Behavior",
    description: "Intentional design, not a bug",
    icon: "Info",
    color: "blue",
    isBuiltIn: true,
    defaultActionType: "informational",
    sortOrder: 6,
  },
  {
    id: "data_migration",
    name: "Data Migration",
    description: "Requires admin or manual intervention",
    icon: "Database",
    color: "purple",
    isBuiltIn: true,
    defaultActionType: "manual",
    sortOrder: 7,
  },
  {
    id: "runtime_issue",
    name: "Runtime Issue",
    description: "Operational issues, not code bugs",
    icon: "Activity",
    color: "yellow",
    isBuiltIn: true,
    defaultActionType: "manual",
    sortOrder: 8,
  },
  {
    id: "test_issue",
    name: "Test Issue",
    description: "Problems with test code or test setup",
    icon: "TestTube",
    color: "purple",
    isBuiltIn: true,
    defaultActionType: "auto_fix",
    sortOrder: 9,
  },
  {
    id: "enhancement",
    name: "Enhancement",
    description: "Improvement suggestions",
    icon: "Sparkles",
    color: "cyan",
    isBuiltIn: true,
    defaultActionType: "needs_user_input",
    sortOrder: 10,
  },
  {
    id: "documentation",
    name: "Documentation",
    description: "Documentation issues or improvements",
    icon: "FileText",
    color: "slate",
    isBuiltIn: true,
    defaultActionType: "auto_fix",
    sortOrder: 11,
  },
  {
    id: "performance",
    name: "Performance",
    description: "Performance issues or optimization opportunities",
    icon: "Zap",
    color: "yellow",
    isBuiltIn: true,
    defaultActionType: "needs_user_input",
    sortOrder: 12,
  },
  {
    id: "warning",
    name: "Warning",
    description: "Things to be aware of",
    icon: "AlertTriangle",
    color: "yellow",
    isBuiltIn: true,
    defaultActionType: "informational",
    sortOrder: 13,
  },
];

/**
 * Default category order (all built-in categories)
 */
const DEFAULT_CATEGORY_ORDER = BUILT_IN_CATEGORIES.map((c) => c.id);

/**
 * Get the category store from localStorage
 */
function getCategoryStore(): CategoryStore {
  try {
    const stored = localStorage.getItem(CATEGORY_STORE_KEY);
    if (stored) {
      return JSON.parse(stored);
    }
  } catch (error) {
    console.error("Failed to load category store:", error);
  }
  return {
    customCategories: [],
    categoryOrder: DEFAULT_CATEGORY_ORDER,
    hiddenCategories: [],
  };
}

/**
 * Save the category store to localStorage
 */
function saveCategoryStore(store: CategoryStore): void {
  try {
    localStorage.setItem(CATEGORY_STORE_KEY, JSON.stringify(store));
  } catch (error) {
    console.error("Failed to save category store:", error);
  }
}

/**
 * Get all categories (built-in + custom), sorted by order
 */
export function getAllCategories(): FindingCategory[] {
  const store = getCategoryStore();
  const allCategories = [...BUILT_IN_CATEGORIES, ...store.customCategories];

  // Sort by the stored order
  const orderMap = new Map(store.categoryOrder.map((id, idx) => [id, idx]));
  return allCategories.sort((a, b) => {
    const orderA = orderMap.get(a.id) ?? a.sortOrder + 1000;
    const orderB = orderMap.get(b.id) ?? b.sortOrder + 1000;
    return orderA - orderB;
  });
}

/**
 * Get visible categories (excludes hidden ones)
 */
export function getVisibleCategories(): FindingCategory[] {
  const store = getCategoryStore();
  const hiddenSet = new Set(store.hiddenCategories);
  return getAllCategories().filter((c) => !hiddenSet.has(c.id));
}

/**
 * Get a category by ID
 */
export function getCategoryById(id: string): FindingCategory | undefined {
  const builtIn = BUILT_IN_CATEGORIES.find((c) => c.id === id);
  if (builtIn) return builtIn;

  const store = getCategoryStore();
  return store.customCategories.find((c) => c.id === id);
}

/**
 * Check if a category ID is built-in
 */
export function isBuiltInCategory(id: string): id is BuiltInCategoryId {
  return BUILT_IN_CATEGORIES.some((c) => c.id === id);
}

/**
 * Add a custom category
 */
export function addCustomCategory(
  category: Omit<FindingCategory, "isBuiltIn" | "sortOrder">,
): FindingCategory {
  const store = getCategoryStore();

  // Ensure unique ID
  if (getCategoryById(category.id) || store.customCategories.some((c) => c.id === category.id)) {
    throw new Error(`Category with ID "${category.id}" already exists`);
  }

  const newCategory: FindingCategory = {
    ...category,
    isBuiltIn: false,
    sortOrder: store.categoryOrder.length + 1,
  };

  store.customCategories.push(newCategory);
  store.categoryOrder.push(category.id);
  saveCategoryStore(store);

  return newCategory;
}

/**
 * Update a custom category (built-in categories cannot be modified)
 */
export function updateCustomCategory(
  id: string,
  updates: Partial<Omit<FindingCategory, "id" | "isBuiltIn">>,
): FindingCategory {
  if (isBuiltInCategory(id)) {
    throw new Error("Cannot modify built-in categories");
  }

  const store = getCategoryStore();
  const index = store.customCategories.findIndex((c) => c.id === id);
  if (index === -1) {
    throw new Error(`Category "${id}" not found`);
  }

  store.customCategories[index] = {
    ...store.customCategories[index],
    ...updates,
  };
  saveCategoryStore(store);

  return store.customCategories[index];
}

/**
 * Delete a custom category (built-in categories cannot be deleted)
 */
export function deleteCustomCategory(id: string): void {
  if (isBuiltInCategory(id)) {
    throw new Error("Cannot delete built-in categories");
  }

  const store = getCategoryStore();
  store.customCategories = store.customCategories.filter((c) => c.id !== id);
  store.categoryOrder = store.categoryOrder.filter((cid) => cid !== id);
  store.hiddenCategories = store.hiddenCategories.filter((cid) => cid !== id);
  saveCategoryStore(store);
}

/**
 * Hide/show a category
 */
export function setCategoryVisibility(id: string, visible: boolean): void {
  const store = getCategoryStore();
  const hiddenSet = new Set(store.hiddenCategories);

  if (visible) {
    hiddenSet.delete(id);
  } else {
    hiddenSet.add(id);
  }

  store.hiddenCategories = Array.from(hiddenSet);
  saveCategoryStore(store);
}

/**
 * Reorder categories
 */
export function setCategoryOrder(orderedIds: string[]): void {
  const store = getCategoryStore();
  store.categoryOrder = orderedIds;
  saveCategoryStore(store);
}

/**
 * Reset to default configuration
 */
export function resetCategories(): void {
  localStorage.removeItem(CATEGORY_STORE_KEY);
}

/**
 * Get Tailwind color classes for a category color
 */
export function getCategoryColorClasses(color: string): {
  bg: string;
  text: string;
  border: string;
  badgeBg: string;
} {
  const colorMap: Record<string, { bg: string; text: string; border: string; badgeBg: string }> = {
    red: {
      bg: "bg-red-500/10",
      text: "text-red-500",
      border: "border-red-500/30",
      badgeBg: "bg-red-500",
    },
    amber: {
      bg: "bg-amber-500/10",
      text: "text-amber-500",
      border: "border-amber-500/30",
      badgeBg: "bg-amber-500",
    },
    orange: {
      bg: "bg-orange-500/10",
      text: "text-orange-500",
      border: "border-orange-500/30",
      badgeBg: "bg-orange-500",
    },
    yellow: {
      bg: "bg-yellow-500/10",
      text: "text-yellow-500",
      border: "border-yellow-500/30",
      badgeBg: "bg-yellow-500",
    },
    green: {
      bg: "bg-green-500/10",
      text: "text-green-500",
      border: "border-green-500/30",
      badgeBg: "bg-green-500",
    },
    blue: {
      bg: "bg-blue-500/10",
      text: "text-blue-500",
      border: "border-blue-500/30",
      badgeBg: "bg-blue-500",
    },
    purple: {
      bg: "bg-purple-500/10",
      text: "text-purple-500",
      border: "border-purple-500/30",
      badgeBg: "bg-purple-500",
    },
    cyan: {
      bg: "bg-cyan-500/10",
      text: "text-cyan-500",
      border: "border-cyan-500/30",
      badgeBg: "bg-cyan-500",
    },
    slate: {
      bg: "bg-slate-500/10",
      text: "text-slate-500",
      border: "border-slate-500/30",
      badgeBg: "bg-slate-500",
    },
  };

  return (
    colorMap[color] || {
      bg: "bg-gray-500/10",
      text: "text-gray-500",
      border: "border-gray-500/30",
      badgeBg: "bg-gray-500",
    }
  );
}
