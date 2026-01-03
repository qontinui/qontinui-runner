/**
 * useContexts.ts
 *
 * Hook for managing AI contexts - provides CRUD operations, filtering,
 * and state management for the context system.
 */

import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ContextWithMetadata,
  ContextScope,
  ContextFilterOptions,
  ContextSelection,
  AutoDetectResult,
  CreateContextRequest,
  UpdateContextRequest,
  WebSyncStatus,
} from "../../../types/context";

interface CommandResponse {
  success: boolean;
  message?: string;
  data?: unknown;
}

const API_BASE = "http://localhost:9876";

interface ApiResponse<T> {
  success: boolean;
  data?: T;
  error?: string;
}

interface UseContextsReturn {
  // Data
  contexts: ContextWithMetadata[];
  categories: string[];
  tags: string[];

  // Filter state
  filters: ContextFilterOptions;
  setFilters: (filters: ContextFilterOptions) => void;
  filteredContexts: ContextWithMetadata[];

  // Loading/error state
  loading: boolean;
  error: string | null;

  // CRUD operations
  createContext: (
    scope: "project" | "user",
    data: CreateContextRequest,
  ) => Promise<ContextWithMetadata | null>;
  updateContext: (
    scope: ContextScope,
    id: string,
    data: UpdateContextRequest,
  ) => Promise<ContextWithMetadata | null>;
  deleteContext: (scope: ContextScope, id: string) => Promise<boolean>;
  duplicateContext: (
    scope: ContextScope,
    id: string,
    targetScope: "project" | "user",
  ) => Promise<ContextWithMetadata | null>;

  // Enable/disable
  enableContext: (id: string) => Promise<boolean>;
  disableContext: (id: string) => Promise<boolean>;

  // Web sync approval (for project contexts)
  approveContextSync: (id: string) => Promise<boolean>;
  dismissContextSync: (id: string) => Promise<boolean>;

  // Refresh
  refresh: () => Promise<void>;

  // Auto-include evaluation (for ContextSelector)
  evaluateAutoInclude: (
    taskPrompt: string,
    actionTypes: string[],
    recentErrors: string[],
  ) => Promise<AutoDetectResult[]>;

  // Record context usage
  recordUsage: (contextIds: string[]) => Promise<void>;

  // Get selected contexts content
  getSelectedContextsContent: (selection: ContextSelection) => string;
}

/**
 * Hook to manage AI contexts
 */
export function useContexts(autoRefresh = false, refreshInterval = 30000): UseContextsReturn {
  const [contexts, setContexts] = useState<ContextWithMetadata[]>([]);
  const [categories, setCategories] = useState<string[]>([]);
  const [tags, setTags] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filters, setFilters] = useState<ContextFilterOptions>({
    scope: "all",
    category: "all",
    searchQuery: "",
    sortBy: "modifiedAt",
    sortDirection: "desc",
    enabledOnly: false,
  });

  const refreshTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  /**
   * Make an API request
   */
  const apiRequest = async <T>(
    method: string,
    endpoint: string,
    body?: unknown,
  ): Promise<T | null> => {
    try {
      const options: RequestInit = {
        method,
        headers: {
          "Content-Type": "application/json",
        },
      };

      if (body) {
        options.body = JSON.stringify(body);
      }

      const response = await fetch(`${API_BASE}${endpoint}`, options);
      const result: ApiResponse<T> = await response.json();

      if (!result.success) {
        throw new Error(result.error || "API request failed");
      }

      return result.data ?? null;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      console.error(`[CONTEXTS] API error: ${message}`);
      throw err;
    }
  };

  /**
   * Load all contexts from all scopes
   */
  const loadContexts = useCallback(async () => {
    try {
      const result = await apiRequest<ContextWithMetadata[]>("GET", "/contexts");
      setContexts(result || []);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to load contexts");
    }
  }, []);

  /**
   * Load available categories
   */
  const loadCategories = useCallback(async () => {
    try {
      const result = await apiRequest<string[]>("GET", "/contexts/categories");
      setCategories(result || []);
    } catch (err) {
      console.warn("[CONTEXTS] Failed to load categories:", err);
    }
  }, []);

  /**
   * Load available tags
   */
  const loadTags = useCallback(async () => {
    try {
      const result = await apiRequest<string[]>("GET", "/contexts/tags");
      setTags(result || []);
    } catch (err) {
      console.warn("[CONTEXTS] Failed to load tags:", err);
    }
  }, []);

  /**
   * Create a new context
   */
  const createContext = useCallback(
    async (
      scope: "project" | "user",
      data: CreateContextRequest,
    ): Promise<ContextWithMetadata | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<ContextWithMetadata>("POST", `/contexts/${scope}`, data);
        if (result) {
          setContexts((prev) => [...prev, result]);
          // Refresh categories and tags
          loadCategories();
          loadTags();
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to create context");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [loadCategories, loadTags],
  );

  /**
   * Update an existing context
   */
  const updateContext = useCallback(
    async (
      scope: ContextScope,
      id: string,
      data: UpdateContextRequest,
    ): Promise<ContextWithMetadata | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<ContextWithMetadata>(
          "PUT",
          `/contexts/${scope}/${id}`,
          data,
        );
        if (result) {
          setContexts((prev) => prev.map((c) => (c.id === id ? result : c)));
          // Refresh categories and tags
          loadCategories();
          loadTags();
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to update context");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [loadCategories, loadTags],
  );

  /**
   * Delete a context
   */
  const deleteContext = useCallback(async (scope: ContextScope, id: string): Promise<boolean> => {
    if (scope === "builtin") {
      setError("Cannot delete built-in contexts");
      return false;
    }

    setLoading(true);
    setError(null);
    try {
      await apiRequest("DELETE", `/contexts/${scope}/${id}`);
      setContexts((prev) => prev.filter((c) => c.id !== id));
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to delete context");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Duplicate a context to a target scope
   */
  const duplicateContext = useCallback(
    async (
      scope: ContextScope,
      id: string,
      targetScope: "project" | "user",
    ): Promise<ContextWithMetadata | null> => {
      setLoading(true);
      setError(null);
      try {
        const result = await apiRequest<ContextWithMetadata>(
          "POST",
          `/contexts/${scope}/${id}/duplicate`,
          { targetScope },
        );
        if (result) {
          setContexts((prev) => [...prev, result]);
        }
        return result;
      } catch (err) {
        setError(err instanceof Error ? err.message : "Failed to duplicate context");
        return null;
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  /**
   * Enable a context
   */
  const enableContext = useCallback(async (id: string): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("POST", `/contexts/metadata/${id}/enable`);
      setContexts((prev) => prev.map((c) => (c.id === id ? { ...c, enabled: true } : c)));
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to enable context");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Disable a context
   */
  const disableContext = useCallback(async (id: string): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("POST", `/contexts/metadata/${id}/disable`);
      setContexts((prev) => prev.map((c) => (c.id === id ? { ...c, enabled: false } : c)));
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to disable context");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Approve syncing a project context to qontinui-web.
   * This will send the context to the web project and mark it as synced.
   */
  const approveContextSync = useCallback(async (id: string): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("POST", `/contexts/${id}/approve-sync`);
      setContexts((prev) =>
        prev.map((c) => (c.id === id ? { ...c, webSyncStatus: "synced" as WebSyncStatus } : c)),
      );
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to sync context to web");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Dismiss syncing a project context to qontinui-web.
   * The context will remain local and won't be synced.
   */
  const dismissContextSync = useCallback(async (id: string): Promise<boolean> => {
    setLoading(true);
    setError(null);
    try {
      await apiRequest("POST", `/contexts/${id}/dismiss-sync`);
      setContexts((prev) =>
        prev.map((c) => (c.id === id ? { ...c, webSyncStatus: "dismissed" as WebSyncStatus } : c)),
      );
      return true;
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to dismiss context sync");
      return false;
    } finally {
      setLoading(false);
    }
  }, []);

  /**
   * Refresh all data
   */
  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      await Promise.all([loadContexts(), loadCategories(), loadTags()]);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to refresh");
    } finally {
      setLoading(false);
    }
  }, [loadContexts, loadCategories, loadTags]);

  /**
   * Filter and sort contexts based on current filter options
   */
  const filteredContexts = useMemo(() => {
    let result = [...contexts];

    // Filter by scope
    if (filters.scope && filters.scope !== "all") {
      result = result.filter((c) => c.scope === filters.scope);
    }

    // Filter by category
    if (filters.category && filters.category !== "all") {
      result = result.filter((c) => c.category === filters.category);
    }

    // Filter by enabled only
    if (filters.enabledOnly) {
      result = result.filter((c) => c.enabled);
    }

    // Filter by search query
    if (filters.searchQuery) {
      const query = filters.searchQuery.toLowerCase();
      result = result.filter(
        (c) =>
          c.name.toLowerCase().includes(query) ||
          c.content.toLowerCase().includes(query) ||
          c.tags?.some((t) => t.toLowerCase().includes(query)) ||
          c.category?.toLowerCase().includes(query),
      );
    }

    // Sort
    const sortBy = filters.sortBy || "modifiedAt";
    const sortDir = filters.sortDirection || "desc";
    const multiplier = sortDir === "asc" ? 1 : -1;

    result.sort((a, b) => {
      switch (sortBy) {
        case "name":
          return a.name.localeCompare(b.name) * multiplier;
        case "createdAt":
          return (new Date(a.createdAt).getTime() - new Date(b.createdAt).getTime()) * multiplier;
        case "modifiedAt":
          return (new Date(a.modifiedAt).getTime() - new Date(b.modifiedAt).getTime()) * multiplier;
        case "useCount":
          return (a.useCount - b.useCount) * multiplier;
        case "lastUsedAt": {
          const aTime = a.lastUsedAt ? new Date(a.lastUsedAt).getTime() : 0;
          const bTime = b.lastUsedAt ? new Date(b.lastUsedAt).getTime() : 0;
          return (aTime - bTime) * multiplier;
        }
        default:
          return 0;
      }
    });

    return result;
  }, [contexts, filters]);

  // Initial load
  useEffect(() => {
    refresh();
  }, [refresh]);

  // Auto-refresh
  useEffect(() => {
    if (!autoRefresh) return;

    refreshTimerRef.current = setInterval(() => {
      loadContexts();
    }, refreshInterval);

    return () => {
      if (refreshTimerRef.current) {
        clearInterval(refreshTimerRef.current);
      }
    };
  }, [autoRefresh, refreshInterval, loadContexts]);

  /**
   * Evaluate auto-include rules for the given task context
   * Uses Tauri command for evaluation
   */
  const evaluateAutoInclude = useCallback(
    async (
      taskPrompt: string,
      actionTypes: string[],
      recentErrors: string[],
    ): Promise<AutoDetectResult[]> => {
      try {
        const response = (await invoke("evaluate_auto_include", {
          taskPrompt,
          actionTypes,
          recentErrors,
        })) as CommandResponse;

        if (response.success && response.data) {
          return response.data as AutoDetectResult[];
        }
        return [];
      } catch (err) {
        console.error("[useContexts] Failed to evaluate auto-include:", err);
        return [];
      }
    },
    [],
  );

  /**
   * Record usage for multiple contexts
   */
  const recordUsage = useCallback(async (contextIds: string[]) => {
    for (const id of contextIds) {
      try {
        await invoke("record_context_usage", { id });
      } catch (err) {
        console.error(`[useContexts] Failed to record usage for ${id}:`, err);
      }
    }
  }, []);

  /**
   * Get the content of selected contexts as a single string
   */
  const getSelectedContextsContent = useCallback(
    (selection: ContextSelection): string => {
      const selectedContexts = contexts.filter((c) => selection.selectedIds.includes(c.id));

      if (selectedContexts.length === 0) {
        return "";
      }

      return selectedContexts.map((c) => `## ${c.name}\n\n${c.content}`).join("\n\n---\n\n");
    },
    [contexts],
  );

  return {
    contexts,
    categories,
    tags,
    filters,
    setFilters,
    filteredContexts,
    loading,
    error,
    createContext,
    updateContext,
    deleteContext,
    duplicateContext,
    enableContext,
    disableContext,
    approveContextSync,
    dismissContextSync,
    refresh,
    evaluateAutoInclude,
    recordUsage,
    getSelectedContextsContent,
  };
}

/**
 * Helper to get contexts grouped by scope
 */
export function groupContextsByScope(contexts: ContextWithMetadata[]): {
  builtin: ContextWithMetadata[];
  user: ContextWithMetadata[];
  project: ContextWithMetadata[];
} {
  return {
    builtin: contexts.filter((c) => c.scope === "builtin"),
    user: contexts.filter((c) => c.scope === "user"),
    project: contexts.filter((c) => c.scope === "project"),
  };
}
